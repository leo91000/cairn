import type { RunEvent } from '../shared/contracts'
import type { HistoryPage, LiveState } from '../shared/live'
import type { ReadingPosition } from './history-cache'
import type { LiveStatus } from './live-connection'
import { computed, onScopeDispose, ref, watch } from 'vue'
import { api, ApiError, state } from './api'
import { cacheScope, clearHistoryCache, readHistory, removeHistory, writeHistory } from './history-cache'
import { liveConnection } from './live-connection'
import { LiveEvents } from './live-events'

// `hold` keeps the previous stream's data on screen for up to that many milliseconds after the
// path changes, so switching views swaps content directly instead of flashing a loading state.
// `shown` is the path whose data is currently displayed.
export function useLiveRun(path: () => string, options: { hold?: number } = {}) {
  const snapshot = ref<LiveState>()
  const shown = ref(path())
  const events = ref<RunEvent[]>([])
  const status = ref<LiveStatus>('connecting')
  const catchingUp = ref(true)
  const synced = ref(false)
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
  let holdTimer: ReturnType<typeof setTimeout> | undefined
  function scheduleSave() {
    if (saveTimer !== undefined)
      return
    saveTimer = setTimeout(() => {
      saveTimer = undefined
      persist?.()
    }, 500)
  }
  function savePosition(value: ReadingPosition, key?: string) {
    if (key && key !== path())
      return
    position.value = value
    scheduleSave()
  }
  function show(value: string) {
    clearTimeout(holdTimer)
    holdTimer = undefined
    shown.value = value
  }
  watch([path, () => state.authenticated && !state.signingOut, () => state.csrf], async ([value, enabled, csrf], previous) => {
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
    const clear = () => {
      show(value)
      hasOlder.value = false
      snapshot.value = undefined
      events.value = []
      catchingUp.value = true
    }
    clearTimeout(holdTimer)
    if (options.hold && enabled && snapshot.value && previous?.[0] !== value && previous?.[1])
      holdTimer = setTimeout(clear, options.hold)
    else
      clear()
    status.value = 'connecting'
    error.value = ''
    if (!enabled) {
      void clearHistoryCache()
      return
    }
    const scope = await cacheScope(csrf).catch(() => undefined)
    const cached = scope ? await readHistory(scope, value) : undefined
    if (disposed || current !== generation)
      return
    let accumulator = new LiveEvents()
    let rows: RunEvent[] = cached?.events.slice() ?? []
    let detail = cached?.state
    let cursor = cached?.cursor ?? 0
    let history = cached?.history
    let oldest = cached?.oldest ?? 0
    // Applied with the complete snapshot, so a held view keeps its own pagination meanwhile.
    let older = cached?.hasOlder ?? false
    let complete = !!cached
    let storedPosition = cached?.position
    position.value = storedPosition
    accumulator.restore(rows, cursor)
    if (cached) {
      show(value)
      hasOlder.value = older
      snapshot.value = cached.state
      events.value = cached.events
      catchingUp.value = false
    }
    persist = () => {
      if (!scope || !complete || !detail || !history || current !== generation || !state.authenticated || state.signingOut)
        return
      storedPosition = position.value
      void writeHistory(scope, value, { version: 1, cursor, history, state: detail, events: rows.slice(), oldest, hasOlder: hasOlder.value, savedAt: Date.now(), position: storedPosition })
    }
    fetchOlder = async () => {
      if (!history || !hasOlder.value || loadingOlder.value)
        return
      const expected = history
      const before = oldest
      loadingOlder.value = true
      olderError.value = ''
      try {
        const page = await api<HistoryPage>(`${value.replace(/\/stream$/, '')}/history?before=${before}&history=${encodeURIComponent(expected)}`)
        if (disposed || current !== generation || history !== expected || page.history !== expected)
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
    connection = liveConnection(value, (batch, accepted) => {
      if (batch.state?.cacheRevision) {
        try {
          if (localStorage.getItem('conversation-cache-revision') !== batch.state.cacheRevision) {
            void clearHistoryCache()
            localStorage.setItem('conversation-cache-revision', batch.state.cacheRevision)
          }
        }
        catch { void clearHistoryCache() }
      }
      const reset = batch.reset || (history && batch.history !== history)
        || (batch.state && detail?.run?.id !== batch.state.run?.id)
      if (reset) {
        rows = []
        accumulator = new LiveEvents()
        complete = false
        position.value = undefined
        olderError.value = ''
        if (scope)
          void removeHistory(scope, value)
      }
      // Persist a complete snapshot only. An interrupted catch-up cannot corrupt it.
      const next = rows.slice()
      const candidate = accumulator.copy()
      candidate.append(next, batch.events)
      accumulator = candidate
      rows = next
      if (batch.state)
        detail = batch.state
      cursor = accepted
      history = batch.history
      if (batch.oldest !== undefined) {
        oldest = batch.oldest
        older = batch.hasOlder ?? false
      }
      complete = !batch.more
      if (complete) {
        show(value)
        hasOlder.value = older
        snapshot.value = detail
        events.value = rows.slice()
        catchingUp.value = false
        synced.value = true
        scheduleSave()
      }
      error.value = ''
    }, (connectionStatus) => {
      status.value = connectionStatus
      if (connectionStatus !== 'reconnecting' || checking)
        return
      checking = true
      void api(value.replace(/\/stream$/, '')).catch((e) => {
        if (!disposed && current === generation && e instanceof ApiError && [401, 403, 404, 409].includes(e.status)) {
          complete = false
          clearTimeout(saveTimer)
          error.value = e.message
          connection?.close()
          show(value)
          snapshot.value = undefined
          events.value = []
          if (scope)
            void removeHistory(scope, value)
        }
      }).finally(() => { checking = false })
    }, cached ? { cursor: cached.cursor, history: cached.history } : undefined)
  }, { immediate: true, flush: 'sync' })
  const leaving = () => persist?.()
  window.addEventListener('pagehide', leaving)
  onScopeDispose(() => {
    window.removeEventListener('pagehide', leaving)
    persist?.()
    clearTimeout(saveTimer)
    clearTimeout(holdTimer)
    disposed = true
    generation++
    connection?.close()
  })
  return {
    snapshot,
    shown,
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
    connectionNotice: computed(() => error.value || shown.value !== path() ? '' : status.value === 'offline' ? (snapshot.value ? 'Offline · showing saved conversation' : 'Offline') : status.value === 'reconnecting' ? 'Reconnecting…' : status.value === 'connecting' && snapshot.value ? 'Updating…' : ''),
    reconnect: () => connection?.reconnect(),
  }
}
