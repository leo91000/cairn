import type { Deliverable } from '../../../packages/contracts/artifacts'
import type { ChatDetail, ChatView } from '../../../packages/contracts/chats'
import type { Run, RunEvent } from '../../../packages/contracts/contracts'
import type { HistoryPage, LiveState } from '../../../packages/contracts/live'
import type { CachedHistory, ReadingPosition } from './history-cache'
import type { LiveStatus } from './live-connection'
import {
  computed,
  onScopeDispose,
  ref,
  watch,
} from 'vue'
import { api, ApiError, state } from './api'
import {
  cacheScope,
  clearHistoryCache,
  knownScope,
  peekHistory,
  readHistory,
  removeHistory,
  writeHistory,
} from './history-cache'
import { liveConnection } from './live-connection'
import { LiveEvents } from './live-events'

/** A revalidation that takes longer than this says so; a quick one stays silent. */
const SLOW_UPDATE = 1000
/** Saving serializes the whole conversation: streaming output saves at most this often. */
const SAVE_DELAY = 2000

export interface LiveRunOptions {
  /**
   * Whether a saved conversation still matches what the server holds. A current one
   * shows at once; an outdated one only seeds the stream's cursor, and the view waits
   * for the result instead of showing older output that is about to change.
   */
  current?: (saved: CachedHistory) => boolean
  /** The stream also carries the conversation list, so the tab needs no second stream. */
  list?: boolean
  /** Where the stream's state is saved, when another path names it better than the one opened. */
  savedAs?: (state: LiveState) => string | undefined
}

// A view shows its own stream only: switching paths clears the previous data at once, so
// a saved conversation shows immediately and anything else waits for its first snapshot.
export function useLiveRun(path: () => string | undefined, options: LiveRunOptions = {}) {
  const snapshot = ref<LiveState>()
  const events = ref<RunEvent[]>([])
  const status = ref<LiveStatus>('connecting')
  const slow = ref(false)
  const catchingUp = ref(true)
  const synced = ref(false)
  // The snapshot shown comes from the saved conversation, not yet from the server.
  const restored = ref(false)
  // A conversation owns the shell's list while connecting, then only while its stream
  // can deliver it. Terminal failures and finite compatibility reads release that job.
  const carriesList = ref(false)
  const error = ref('')
  const position = ref<ReadingPosition>()
  const hasOlder = ref(false)
  const loadingOlder = ref(false)
  const olderError = ref('')
  let fetchOlder: (() => Promise<void>) | undefined
  const loadOlder = () => fetchOlder?.()
  let connection: ReturnType<typeof liveConnection> | undefined
  let disposed = false
  let generation = 0
  let persist: (() => void) | undefined
  let saveTimer: ReturnType<typeof setTimeout> | undefined
  let slowTimer: ReturnType<typeof setTimeout> | undefined

  function scheduleSave() {
    if (saveTimer !== undefined)
      return
    saveTimer = setTimeout(() => {
      saveTimer = undefined
      persist?.()
    }, SAVE_DELAY)
  }

  function savePosition(value: ReadingPosition, key?: string) {
    if (key && key !== path())
      return
    position.value = value
    scheduleSave()
  }

  function setStatus(value: LiveStatus) {
    status.value = value
    clearTimeout(slowTimer)
    slow.value = false
    if (value === 'connecting')
      slowTimer = setTimeout(() => slow.value = true, SLOW_UPDATE)
  }

  async function open(value: string, current: number, session: string) {
    // A conversation read earlier in this page shows in the same task, before any paint.
    let scope = knownScope(session)
    let cached = scope ? peekHistory(scope, value) : undefined
    if (!cached) {
      scope = await cacheScope(session).catch(() => undefined)
      cached = scope ? await readHistory(scope, value) : undefined
      if (disposed || current !== generation)
        return
    }

    let accumulator = new LiveEvents()
    let rows: RunEvent[] = cached?.events.slice() ?? []
    let detail = cached?.state
    let cursor = cached?.cursor ?? 0
    let history = cached?.history
    let oldest = cached?.oldest ?? 0
    // Applied with the complete snapshot, so a resumed view keeps its own pagination meanwhile.
    let older = cached?.hasOlder ?? false
    let complete = !!cached
    // Counts history replacements: a page requested before one no longer joins the rows.
    let replacements = 0
    let storedPosition = cached?.position
    position.value = storedPosition
    accumulator.restore(rows, cursor)

    function showSaved(saved: CachedHistory) {
      hasOlder.value = older
      restored.value = true
      snapshot.value = saved.state
      events.value = saved.events
      catchingUp.value = false
    }

    let waiting = !!cached && options.current?.(cached) === false
    if (cached && !waiting)
      showSaved(cached)

    // A copy opened under another path is also saved where other openings look for it.
    const saved = () => [...new Set([value, (detail && options.savedAs?.(detail)) || value])]
    persist = () => {
      if (!scope || !complete || !detail || !history || current !== generation || !state.authenticated || state.signingOut)
        return
      storedPosition = position.value
      const record = {
        version: 1 as const,
        cursor,
        history,
        state: detail,
        events: rows.slice(),
        oldest,
        hasOlder: older,
        savedAt: Date.now(),
        position: storedPosition,
      }
      for (const path of saved())
        void writeHistory(scope, path, record)
    }

    fetchOlder = async () => {
      if (!history || !hasOlder.value || loadingOlder.value)
        return
      const expected = history
      const before = oldest
      const baseline = replacements
      loadingOlder.value = true
      olderError.value = ''
      try {
        const page = await api<HistoryPage>(`${value.replace(/\/stream$/, '')}/history?before=${before}&history=${encodeURIComponent(expected)}`)
        if (disposed || current !== generation || history !== expected || page.history !== expected || replacements !== baseline)
          return
        if (page.hasOlder && page.oldest >= before)
          throw new Error('Invalid history page')
        const next: RunEvent[] = []
        const candidate = new LiveEvents()
        candidate.append(next, [...new Map([...page.events, ...rows].map(e => [e.id, e])).values()].sort((a, b) => a.id - b.id))
        candidate.restore(next, cursor)
        accumulator = candidate
        rows = next
        oldest = page.oldest
        older = page.hasOlder
        hasOlder.value = older
        events.value = rows.slice()
        scheduleSave()
      }
      catch (e) {
        if (current === generation)
          olderError.value = e instanceof Error ? e.message : 'Unable to load history'
      }
      finally {
        if (current === generation)
          loadingOlder.value = false
      }
    }

    let checking = false
    let receivedBatches = 0
    let readingSnapshot = false
    let readSnapshotOnce = false
    let finiteRead = false

    // An installation on the first relay protocol cannot stream: when the stream fails
    // before its first batch, one finite read keeps it readable. Never polled.
    async function readSnapshot() {
      if (readingSnapshot || readSnapshotOnce || receivedBatches || document.hidden || disposed || current !== generation)
        return
      readingSnapshot = true
      readSnapshotOnce = true
      try {
        const endpoint = value.replace(/\/stream$/, '')
        const isList = endpoint === '/chats'
        const isChat = endpoint.startsWith('/chats/')
        const result = await api<ChatDetail | ChatView[] | Run>(endpoint)
        const chat = isChat ? result as ChatDetail : null
        const run = isList ? null : isChat ? chat!.run : result as Run
        const nextRows: RunEvent[] = []
        const fallback = new LiveEvents()
        let files: Deliverable[] = []
        if (run?.id) {
          files = await api<Deliverable[]>(`/runs/${run.id}/artifacts`)
          let page: RunEvent[]
          do {
            page = await api<RunEvent[]>(`/runs/${run.id}/events?after=${fallback.cursor}&limit=500`)
            fallback.append(nextRows, page)
            if (disposed || current !== generation || receivedBatches)
              return
          } while (page.length === 500)
        }

        if (disposed || current !== generation || receivedBatches)
          return
        // HTTP reads have no validated SSE history/cursor and must not replace
        // the stream's persisted snapshot or advance its replay cursor.
        complete = false
        finiteRead = true
        carriesList.value = false
        restored.value = false
        snapshot.value = {
          chat,
          run,
          artifacts: files,
          ...(isList ? { chats: result as ChatView[] } : {}),
        }
        events.value = nextRows
        catchingUp.value = false
        hasOlder.value = false
        error.value = ''
      }
      catch (cause) {
        if (!disposed && current === generation && !receivedBatches) {
          error.value = cause instanceof Error ? cause.message : 'Unable to reach your installation.'
          catchingUp.value = false
        }
      }
      finally {
        readingSnapshot = false
      }
    }

    connection = liveConnection(value, (batch, accepted) => {
      receivedBatches++
      if (batch.state?.cacheRevision) {
        const revisionKey = state.installationId ? `conversation-cache-revision:${state.installationId}` : 'conversation-cache-revision'
        try {
          if (localStorage.getItem(revisionKey) !== batch.state.cacheRevision) {
            void clearHistoryCache()
            localStorage.setItem(revisionKey, batch.state.cacheRevision)
          }
        }
        catch { void clearHistoryCache() }
      }

      const reset = batch.reset || (history && batch.history !== history)
        || (batch.state && detail?.run?.id !== batch.state.run?.id)
      if (reset) {
        replacements++
        rows = []
        accumulator = new LiveEvents()
        oldest = 0
        older = false
        complete = false
        position.value = undefined
        olderError.value = ''
        if (scope)
          saved().forEach(path => void removeHistory(scope, path))
      }

      // Persist a complete snapshot only. An interrupted catch-up cannot corrupt it.
      const next = rows.slice()
      const candidate = accumulator.copy()
      candidate.append(next, batch.events)
      accumulator = candidate
      rows = next
      if (batch.state)
        detail = batch.state
      finiteRead = false
      carriesList.value = !!options.list && !!detail?.chats

      cursor = accepted
      history = batch.history
      // Revalidation describes the server's recent window, not the pages already read.
      // Keep the earliest loaded boundary unless the stream replaces the history.
      if (batch.oldest !== undefined && (reset || !oldest || batch.oldest <= oldest)) {
        oldest = batch.oldest
        older = batch.hasOlder ?? false
      }

      complete = !batch.more
      if (complete) {
        waiting = false
        hasOlder.value = older
        synced.value = true
        if (batch.state)
          restored.value = false
        snapshot.value = detail
        events.value = rows.slice()
        catchingUp.value = false
        scheduleSave()
      }

      error.value = ''
    }, (connectionStatus, opening = false) => {
      // A finite read lets the shell carry the Fil while streaming is unavailable. Each
      // retry takes the slot back before connecting, keeping one stream per tab even
      // when every account slot is occupied; a failed retry gives it back again.
      if (options.list && (opening || finiteRead))
        carriesList.value = opening
      setStatus(connectionStatus)
      if (connectionStatus === 'live')
        return
      // Without a connection, an outdated saved conversation is still better than nothing.
      if (waiting && cached && connectionStatus !== 'connecting') {
        waiting = false
        showSaved(cached)
      }

      if (connectionStatus !== 'reconnecting')
        return
      if (state.installationId && (!cached || value === '/chats/stream'))
        void readSnapshot()
      if (checking)
        return
      checking = true
      void api(value.replace(/\/stream$/, '')).catch((e) => {
        if (!disposed && current === generation && e instanceof ApiError && [401, 403, 404, 409].includes(e.status)) {
          complete = false
          clearTimeout(saveTimer)
          error.value = e.message
          carriesList.value = false
          connection?.close()
          snapshot.value = undefined
          events.value = []
          if (scope)
            saved().forEach(path => void removeHistory(scope, path))
        }
      }).finally(() => { checking = false })
    }, cached ? { cursor: cached.cursor, history: cached.history } : undefined, () => position.value?.follow ?? true, options.list ? '&list=1' : '')
  }

  watch([path, () => state.authenticated && !state.signingOut, () => state.csrf], ([value, enabled, csrf]) => {
    persist?.()
    const current = ++generation
    connection?.close()
    clearTimeout(saveTimer)
    saveTimer = undefined
    persist = undefined
    fetchOlder = undefined
    loadingOlder.value = false
    olderError.value = ''
    position.value = undefined
    synced.value = false
    restored.value = false
    hasOlder.value = false
    snapshot.value = undefined
    events.value = []
    catchingUp.value = true
    setStatus('connecting')
    error.value = ''
    carriesList.value = !!value && enabled && !!options.list
    if (!enabled) {
      void clearHistoryCache()
      return
    }

    if (value)
      void open(value, current, state.installationId ? `${state.installationId}:${csrf}` : csrf)
  }, { immediate: true, flush: 'sync' })
  const leaving = () => persist?.()
  const hidden = () => document.hidden && persist?.()
  window.addEventListener('pagehide', leaving)
  document.addEventListener('visibilitychange', hidden)
  onScopeDispose(() => {
    window.removeEventListener('pagehide', leaving)
    document.removeEventListener('visibilitychange', hidden)
    persist?.()
    clearTimeout(saveTimer)
    clearTimeout(slowTimer)
    disposed = true
    generation++
    connection?.close()
  })
  return {
    snapshot,
    restored,
    carriesList,
    hasOlder,
    loadingOlder,
    olderError,
    loadOlder,
    events,
    catchingUp,
    synced,
    error,
    position,
    savePosition,
    connectionNotice: computed(() => error.value ? '' : status.value === 'offline' ? (snapshot.value ? 'Offline · showing saved conversation' : 'Offline') : status.value === 'reconnecting' ? 'Reconnecting…' : status.value === 'connecting' && snapshot.value && slow.value ? 'Updating…' : ''),
    reconnect: () => connection?.reconnect(),
  }
}
