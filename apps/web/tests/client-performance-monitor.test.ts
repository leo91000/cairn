import type { PerformanceEnvironment } from '../src/client-performance-monitor'
import { expect, it } from 'vitest'
import { ClientPerformance } from '../src/client-performance'
import { monitorPerformance } from '../src/client-performance-monitor'

function fixture(supported = true) {
  let now = 0
  let visible = true
  let visibility: (() => void) | undefined
  let activity: (() => void) | undefined
  const timers = new Set<() => void>()
  const observers: Array<(at: number, duration: number) => void> = []
  let observed = 0
  const store = new ClientPerformance(() => now, () => '00000000-0000-4000-8000-000000000001')
  const environment: PerformanceEnvironment = {
    now: () => now,
    visible: () => visible,
    onVisibility: (listener) => {
      visibility = listener
      return () => visibility = undefined
    },
    onActivity: (listener) => {
      activity = listener
      return () => activity = undefined
    },
    schedule: (callback) => {
      timers.add(callback)
      return () => {
        timers.delete(callback)
      }
    },
    observeLongTasks: (record) => {
      observers.push(record)
      if (supported)
        observed++
      return {
        supported,
        stop: () => {
          if (supported)
            observed--
        },
      }
    },
  }
  const monitor = monitorPerformance(store, environment)
  return {
    store,
    monitor,
    timers,
    observers,
    advance: (duration: number) => now += duration,
    activity: () => activity?.(),
    visible: (next: boolean) => {
      visible = next
      visibility?.()
    },
    flush: () => {
      for (const callback of [...timers]) {
        timers.delete(callback)
        callback()
      }
    },
    observed: () => observed,
    listening: () => !!visibility || !!activity,
    report: () => store.report({ buildId: 'a'.repeat(64), buildType: 'release', browser: 'SAFARI' }, 'NONE')!,
  }
}

it('uses finite activity probes, coalesces bursts and stays idle after completion', () => {
  const f = fixture()
  f.advance(60)
  f.flush()
  expect(f.report().measurements.eventLoop[0]).toMatchObject({
    reason: 'NAVIGATION',
    count: 1,
    late: 1,
    maxDelayMs: 60,
  })
  expect(f.timers.size).toBe(0)
  f.activity()
  expect(f.timers.size).toBe(0)
  f.advance(500)
  f.activity()
  f.activity()
  f.store.probe('API')
  expect(f.timers.size).toBe(1)
  f.advance(80)
  f.flush()
  expect(f.timers.size).toBe(0)
  expect(f.report().measurements.eventLoop).toEqual(expect.arrayContaining([
    expect.objectContaining({
      reason: 'INTERACTION',
      count: 1,
      late: 1,
      maxDelayMs: 80,
    }),
  ]))
})

it('cancels foreground observations and prevents old callbacks entering a reset window', () => {
  const f = fixture()
  const oldObserver = f.observers[0]!
  expect(f.observed()).toBe(1)
  f.visible(false)
  expect(f.observed()).toBe(0)
  expect(f.timers.size).toBe(0)
  f.advance(1000)
  oldObserver(0, 100)
  f.visible(true)
  f.monitor.clear()
  oldObserver(0, 100)
  expect(f.report().measurements.longTasks).toEqual([])
  expect(f.observed()).toBe(1)
  f.advance(100)
  f.observers.at(-1)!(1000, 70)
  expect(f.report().measurements.longTasks[0]).toMatchObject({ count: 1, totalMs: 70 })
  f.monitor.stop()
  f.monitor.stop()
  expect(f.observed()).toBe(0)
  expect(f.timers.size).toBe(0)
  expect(f.listening()).toBe(false)
  f.activity()
  f.store.probe('API')
  expect(f.timers.size).toBe(0)
})

it('retains event-loop observations when long tasks are unsupported', () => {
  const f = fixture(false)
  f.advance(100)
  f.flush()
  expect(f.report().measurements.longTaskSupported).toBe(false)
  expect(f.report().measurements.longTasks).toEqual([])
  expect(f.report().measurements.eventLoop[0]!.maxDelayMs).toBe(100)
  f.monitor.stop()
})
