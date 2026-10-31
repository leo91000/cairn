import type { LiveBatch } from '../../../packages/contracts/live'
import { watch } from 'vue'
import { apiUrl, installationTransport, state } from './api'
import { ProtocolMismatch } from './direct-channel'
import { observeTransport } from './transport-observation'

export type LiveStatus = 'connecting' | 'live' | 'reconnecting' | 'offline' | 'updating'

// The cursor advances only after the consumer accepts a complete batch. Every
// connection owns its cursor, optionally restored with a matching cached snapshot.
// A viewer following the latest output accepts the recent window in place of a long replay.
// `query` adds request parameters, such as `&list=1` for a stream that carries the list.
// A status with `opening` is sent before creating the source, so a caller can release
// another stream that temporarily occupies this one's slot.
export function liveConnection(path: string, accept: (batch: LiveBatch, cursor: number, signal?: AbortSignal) => void | Promise<void>, status: (value: LiveStatus, opening?: boolean) => void, initial?: { cursor: number, history: string }, followsLatest?: () => boolean, query = '') {
  let cursor = String(initial?.cursor ?? 0)
  let history = initial?.history
  let source: EventSource | undefined
  let timer: ReturnType<typeof setTimeout> | undefined
  let watchdog: ReturnType<typeof setTimeout> | undefined
  let stopped = false
  let failures = 0
  let resetConsumer = false
  let waitingForTimeline = false
  let abort: AbortController | undefined

  function disconnect() {
    abort?.abort()
    abort = undefined
    source?.close()
    source = undefined
    clearTimeout(timer)
    clearTimeout(watchdog)
  }

  function retry(awaitingTimeline = false) {
    waitingForTimeline = awaitingTimeline
    disconnect()
    if (stopped)
      return
    status(navigator.onLine ? waitingForTimeline ? 'updating' : 'reconnecting' : 'offline')
    if (navigator.onLine)
      timer = setTimeout(connect, Math.min(15000, 500 * 2 ** Math.min(failures++, 5)))
  }

  function alive() {
    clearTimeout(watchdog)
    watchdog = setTimeout(retry, 45000)
  }

  function connect() {
    disconnect()
    if (stopped || state.redirecting)
      return
    if (!navigator.onLine) {
      status('offline')
      return
    }

    status(waitingForTimeline ? 'updating' : failures ? 'reconnecting' : 'connecting', true)
    const windowed = path !== '/chats/stream'
    const recent = windowed && cursor !== '0' && followsLatest?.()
    const route = `${path}?after=${cursor}${history ? `&history=${encodeURIComponent(history)}` : ''}${windowed ? '&window=1&timeline=1' : ''}${recent ? '&recent=1' : ''}${query}`
    let current: EventSource
    try {
      current = installationTransport.source(route, apiUrl(route))
    }
    catch (error) {
      if (error instanceof ProtocolMismatch) {
        stopped = true
        status('offline')
        return
      }

      retry()
      return
    }

    source = current
    const controller = new AbortController()
    abort = controller
    let pending: Promise<void> | undefined
    alive()
    current.addEventListener('ping', () => {
      if (source === current && !stopped)
        alive()
    })
    const process = (event: MessageEvent): void | Promise<void> => {
      if (source !== current || stopped)
        return
      try {
        if (!/^\d+$/.test(event.lastEventId))
          throw new Error('Invalid stream cursor')
        const batch = JSON.parse(event.data) as LiveBatch
        if (!Array.isArray(batch.events) || typeof batch.reset !== 'boolean' || typeof batch.more !== 'boolean')
          throw new Error('Invalid stream batch')
        if (windowed && batch.history?.startsWith('v1:')) {
          retry(true)
          return
        }

        if (history && !batch.history) {
          cursor = '0'
          history = undefined
          throw new Error('Server no longer supports history validation')
        }

        const accepted = () => {
          if (source !== current || stopped)
            return
          resetConsumer = false
          waitingForTimeline = false
          const transport = (current as EventSource & { transportRoute?: 'direct' | 'relay' }).transportRoute
          if (transport)
            observeTransport(transport, path, 'STREAM', Number(event.lastEventId))
          cursor = event.lastEventId
          history = batch.history
          failures = 0
          status('live')
          alive()
        }

        const rejected = () => {
          if (source !== current || stopped)
            return
          cursor = '0'
          history = undefined
          resetConsumer = true
          retry()
        }

        try {
          const result = accept(resetConsumer ? { ...batch, reset: true } : batch, Number(event.lastEventId), controller.signal)
          if (result)
            return result.then(accepted).catch(rejected)
          accepted()
        }
        catch {
          rejected()
        }
      }
      catch {
        if (source === current && !stopped)
          retry()
      }
    }

    current.addEventListener('batch', (event) => {
      if (source !== current || stopped)
        return
      const result = pending ? pending.then(() => process(event)) : process(event)
      if (result) {
        const task = Promise.resolve(result)
        pending = task
        void task.finally(() => {
          if (pending === task)
            pending = undefined
        })
      }
    })
    current.onerror = () => {
      if (source === current)
        retry()
    }
  }

  function visible() {
    if (!document.hidden)
      connect()
  }

  function offline() {
    disconnect()
    status('offline')
  }

  const stopNavigation = watch(() => state.redirecting, connect, { flush: 'sync' })
  window.addEventListener('cairn-transport-change', connect)
  window.addEventListener('online', connect)
  window.addEventListener('offline', offline)
  window.addEventListener('pageshow', visible)
  document.addEventListener('visibilitychange', visible)
  connect()
  return {
    reconnect: connect,
    close() {
      stopped = true
      disconnect()
      stopNavigation()
      window.removeEventListener('cairn-transport-change', connect)
      window.removeEventListener('online', connect)
      window.removeEventListener('offline', offline)
      window.removeEventListener('pageshow', visible)
      document.removeEventListener('visibilitychange', visible)
    },
  }
}
