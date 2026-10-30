import type { ClientPerformance, ProbeReason } from './client-performance'

type Stop = () => void

export interface PerformanceEnvironment {
  now: () => number
  visible: () => boolean
  onVisibility: (changed: () => void) => Stop
  onActivity: (activity: () => void) => Stop
  schedule: (callback: () => void, milliseconds: number) => Stop
  observeLongTasks: (record: (at: number, duration: number) => void) => { supported: boolean, stop: Stop }
}

/** Browser effects are owned together so account resets and disposal cancel old observations. */
export function browserPerformanceEnvironment(): PerformanceEnvironment {
  let pageHidden = false
  return {
    now: () => performance.now(),
    visible: () => !pageHidden && document.visibilityState === 'visible',
    onVisibility: (changed) => {
      const hide = () => {
        pageHidden = true
        changed()
      }

      const show = () => {
        pageHidden = false
        changed()
      }

      document.addEventListener('visibilitychange', changed)
      window.addEventListener('pagehide', hide)
      window.addEventListener('pageshow', show)
      return () => {
        document.removeEventListener('visibilitychange', changed)
        window.removeEventListener('pagehide', hide)
        window.removeEventListener('pageshow', show)
      }
    },
    onActivity: (activity) => {
      // No keys, coordinates, input text or event object is retained.
      for (const type of ['pointerdown', 'keydown', 'wheel'])
        document.addEventListener(type, activity, { passive: true, capture: true })
      return () => {
        for (const type of ['pointerdown', 'keydown', 'wheel'])
          document.removeEventListener(type, activity, { capture: true })
      }
    },
    schedule: (callback, milliseconds) => {
      const timer = window.setTimeout(callback, milliseconds)
      return () => window.clearTimeout(timer)
    },
    observeLongTasks: (record) => {
      if (typeof PerformanceObserver === 'undefined' || !PerformanceObserver.supportedEntryTypes?.includes('longtask'))
        return { supported: false, stop: () => {} }
      try {
        const observer = new PerformanceObserver((list) => {
          for (const entry of list.getEntries()) {
            if (entry.entryType === 'longtask')
              record(entry.startTime, entry.duration)
          }
        })
        // Do not replay entries from a previous account or foreground interval.
        observer.observe({ type: 'longtask', buffered: false })
        return { supported: true, stop: () => observer.disconnect() }
      }
      catch { return { supported: false, stop: () => {} } }
    },
  }
}

export function monitorPerformance(
  store: ClientPerformance,
  environment = browserPerformanceEnvironment(),
  foregroundChanged: (visible: boolean) => void = () => {},
) {
  let stopped = false
  let generation = 0
  let lastProbe = -Infinity
  let cancelProbe: Stop | undefined
  let stopObserver: Stop = () => {}

  function probe(reason: ProbeReason) {
    const scope = store.capture()
    const at = environment.now()
    if (stopped || !scope || !environment.visible() || cancelProbe || at - lastProbe < 500)
      return
    lastProbe = at
    // One finite yield probe after real activity; no idle polling or permanent RAF loop.
    cancelProbe = environment.schedule(() => {
      cancelProbe = undefined
      if (!stopped && environment.visible())
        store.recordProbe(scope, reason, environment.now() - at)
    }, 0)
  }

  function synchronize() {
    cancelProbe?.()
    cancelProbe = undefined
    const observerGeneration = ++generation
    stopObserver()
    stopObserver = () => {}
    const visible = environment.visible()
    store.setActive(visible)
    foregroundChanged(visible)
    if (!visible)
      return
    const observer = environment.observeLongTasks((at, duration) => {
      if (!stopped && observerGeneration === generation)
        store.recordLongTask(at, duration)
    })
    store.setLongTaskSupport(observer.supported)
    stopObserver = observer.stop
    probe('NAVIGATION')
  }

  store.setProbeScheduler(probe)
  const stopVisibility = environment.onVisibility(synchronize)
  const stopActivity = environment.onActivity(() => probe('INTERACTION'))
  synchronize()

  return {
    clear() {
      if (stopped)
        return
      store.clear()
      lastProbe = -Infinity
      synchronize()
    },
    stop() {
      if (stopped)
        return
      stopped = true
      generation++
      cancelProbe?.()
      stopObserver()
      stopVisibility()
      stopActivity()
      store.setProbeScheduler(undefined)
      store.setActive(false)
      foregroundChanged(false)
    },
  }
}
