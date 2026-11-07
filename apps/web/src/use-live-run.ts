import type { RunEvent } from '../../../packages/contracts/contracts'
import type { HistoryPage, LiveState, TimelineEntry } from '../../../packages/contracts/live'
import type { CachedHistory, ReadingPosition } from './history-cache'
import type { LiveStatus } from './live-connection'
import {
  computed,
  onScopeDispose,
  ref,
  watch,
} from 'vue'
import { readRunHistory } from '../../../packages/contracts/generated/readRunHistory'
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
import { readLargeEvents, readLargeTimeline } from './large-timeline'
import { liveConnection } from './live-connection'
import { LiveTimeline } from './live-timeline'

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
  /** Timeline cards, in the order they first appeared. */
  const timeline = ref<TimelineEntry[]>([])
  const status = ref<LiveStatus>('connecting')
  const slow = ref(false)
  const catchingUp = ref(true)
  const synced = ref(false)
  // The snapshot shown comes from the saved conversation, not yet from the server.
  const restored = ref(false)
  // A conversation owns the shell's list while connecting, then only while its stream
  // can deliver it. Terminal failures release that job.
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

    let accumulator = new LiveTimeline()
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
    accumulator.restore(cached?.events ?? [], cursor, cached?.entries ?? [])

    function show() {
      events.value = accumulator.events.slice()
      timeline.value = accumulator.entries()
    }

    function showSaved(saved: CachedHistory) {
      hasOlder.value = older
      restored.value = true
      snapshot.value = saved.state
      events.value = saved.events
      timeline.value = saved.entries
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
        version: 2 as const,
        cursor,
        history,
        state: detail,
        events: accumulator.events.slice(),
        entries: accumulator.entries(),
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
        const runId = /^\/runs\/([^/]+)\/stream$/.exec(value)?.[1]
        const page = runId
          ? await readRunHistory(api, { runId, before, history: expected })
          : await api<HistoryPage>(`${value.replace(/\/stream$/, '')}/history?before=${before}&history=${encodeURIComponent(expected)}`)
        const large = await readLargeTimeline(value, page.largeEntries ?? [])
        const events = await readLargeEvents(value, page.events)
        if (disposed || current !== generation || history !== expected || page.history !== expected || replacements !== baseline)
          return
        if (page.hasOlder && page.oldest >= before)
          throw new Error('Invalid history page')
        const candidate = accumulator.copy()
        candidate.merge(events, [...page.entries, ...large])
        accumulator = candidate
        oldest = page.oldest
        older = page.hasOlder
        hasOlder.value = older
        show()
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
    connection = liveConnection(value, (batch, accepted, signal) => {
      const apply = (large: TimelineEntry[], events = batch.events) => {
        if (signal?.aborted || disposed || current !== generation)
          return
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
          accumulator = new LiveTimeline()
          oldest = 0
          older = false
          complete = false
          position.value = undefined
          olderError.value = ''
          if (scope)
            saved().forEach(path => void removeHistory(scope, path))
        }

        // Persist a complete snapshot only. An interrupted catch-up cannot corrupt it.
        const candidate = accumulator.copy()
        candidate.apply(events, [...(batch.entries ?? []), ...large.map(entry => ({ kind: 'upsert' as const, entry }))], accepted)
        accumulator = candidate
        if (batch.state)
          detail = batch.state
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
          show()
          catchingUp.value = false
          scheduleSave()
        }

        error.value = ''
      }

      if (batch.largeEntries?.length || batch.events.some(event => event.payload?._cairnEventContent === true)) {
        return Promise.all([
          readLargeTimeline(value, batch.largeEntries ?? [], signal),
          readLargeEvents(value, batch.events, signal),
        ]).then(([large, events]) => apply(large, events))
      }

      apply([])
    }, (connectionStatus, opening = false) => {
      if (options.list && opening)
        carriesList.value = true
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
          timeline.value = []
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
    timeline.value = []
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
    timeline,
    catchingUp,
    synced,
    error,
    position,
    savePosition,
    connectionNotice: computed(() => error.value ? '' : status.value === 'offline' ? (snapshot.value ? 'Offline · showing saved conversation' : 'Offline') : status.value === 'reconnecting' ? 'Reconnecting…' : status.value === 'connecting' && snapshot.value && slow.value ? 'Updating…' : ''),
    reconnect: () => connection?.reconnect(),
  }
}
