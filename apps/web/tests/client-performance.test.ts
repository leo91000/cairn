import type {
  PerformanceOperation,
  PerformanceOutcome,
  ProbeReason,
  WebBuild,
} from '../src/client-performance'
import { expect, it } from 'vitest'
import { ClientPerformance, performanceOperations, performanceScreens } from '../src/client-performance'

const build: WebBuild = { buildId: 'a'.repeat(64), buildType: 'release', browser: 'FIREFOX' }

function fixture() {
  let now = 0
  let id = 0
  const store = new ClientPerformance(() => now, () => `00000000-0000-4000-8000-${(++id).toString().padStart(12, '0')}`)
  return { store, advance: (duration: number) => now += duration, report: () => store.report(build, 'RELAY')! }
}

it('publishes increasing pending age while completed-only idle snapshots remain deduplicated', () => {
  const f = fixture()
  f.store.setActive(true)
  const finish = f.store.begin('API_READ')!
  const first = f.report()
  f.advance(60_000)
  const second = f.report()
  expect(second.measurements.sequence).toBeGreaterThan(first.measurements.sequence)
  expect(second.measurements.inFlight[0]!.maxElapsedMs).toBe(60_000)
  finish('ok')
  const completed = f.report()
  f.advance(60_000)
  expect(f.report().measurements.sequence).toBe(completed.measurements.sequence)
})

it('keeps completed, failed, cancelled and in-flight operations separate without payloads', () => {
  const f = fixture()
  f.store.setActive(true)
  f.store.setScreen('CHAT')
  const failed = f.store.begin('API_READ')!
  const cancelled = f.store.begin('API_WRITE')!
  f.advance(75)
  expect(f.report().measurements.inFlight).toEqual([
    {
      operation: 'API_READ',
      screen: 'CHAT',
      count: 1,
      maxElapsedMs: 75,
    },
    {
      operation: 'API_WRITE',
      screen: 'CHAT',
      count: 1,
      maxElapsedMs: 75,
    },
  ])
  failed('error')
  cancelled('cancelled')
  failed('ok')
  const data = f.report().measurements
  expect(data.operations).toHaveLength(2)
  expect(data.operations[0]).toMatchObject({
    operation: 'API_READ',
    count: 1,
    meanMs: 75,
    maxMs: 75,
    p95UpperBoundMs: 75,
  })
  expect(data.apiFailures).toEqual([
    {
      operation: 'API_READ',
      screen: 'CHAT',
      errors: 1,
      cancelled: 0,
    },
    {
      operation: 'API_WRITE',
      screen: 'CHAT',
      errors: 0,
      cancelled: 1,
    },
  ])
  expect(data.inFlight).toEqual([])
})

it('drops work crossing account/reset or foreground boundaries', () => {
  const f = fixture()
  f.store.setActive(true)
  const previous = f.report().measurements.windowId
  const oldAccount = f.store.begin('API_READ')!
  f.advance(20)
  f.store.clear()
  oldAccount('error')
  expect(f.report().measurements.windowId).not.toBe(previous)
  expect(f.report().measurements.operations).toEqual([])
  const hidden = f.store.begin('API_READ')!
  f.advance(10)
  f.store.setActive(false)
  f.advance(1000)
  expect(f.store.begin('API_READ')).toBeNull()
  f.store.setActive(true)
  f.advance(5)
  hidden('ok')
  expect(f.report().measurements.operations).toEqual([])
  expect(f.report().measurements.inFlight).toEqual([])
  expect(f.report().measurements.foregroundMs).toBe(15)
})

it('keeps real return values and thrown errors while recording synchronous main-thread work', () => {
  const f = fixture()
  f.store.setActive(true)
  const value = { privateText: 'never copy content' }
  expect(f.store.measure('TIMELINE', () => {
    f.advance(80)
    return value
  })).toBe(value)
  const failure = new Error('private exception text')
  expect(() => f.store.measure('MARKDOWN', () => {
    f.advance(100)
    throw failure
  })).toThrow(failure)
  const encoded = JSON.stringify(f.report())
  expect(encoded).not.toContain('never copy content')
  expect(encoded).not.toContain('private exception text')
  expect(f.report().measurements.operations).toEqual(expect.arrayContaining([
    expect.objectContaining({ operation: 'TIMELINE', count: 1, meanMs: 80 }),
    expect.objectContaining({ operation: 'MARKDOWN', count: 1, meanMs: 100 }),
  ]))
})

it('attributes late long-task delivery to the start screen and rejects hidden/old samples', () => {
  const f = fixture()
  f.store.setActive(true)
  f.store.setLongTaskSupport(true)
  f.store.setScreen('CHAT')
  f.advance(80)
  f.store.setScreen('ATELIER')
  f.advance(40)
  f.store.recordLongTask(10, 90)
  expect(f.report().measurements.longTasks).toEqual([{
    screen: 'CHAT',
    count: 1,
    totalMs: 90,
    maxMs: 90,
  }])
  f.store.setActive(false)
  f.advance(100)
  f.store.setActive(true)
  f.store.recordLongTask(10, 90)
  expect(f.report().measurements.longTasks[0]!.count).toBe(1)
  f.store.clear()
  f.store.recordLongTask(10, 90)
  expect(f.report().measurements.longTasks).toEqual([])
})

it('bounds categories, pending tracking, samples and the serialized upload', () => {
  const f = fixture()
  f.store.setActive(true)
  f.store.setLongTaskSupport(true)
  const finish = []
  for (const screen of performanceScreens) {
    f.store.setScreen(screen)
    for (const operation of performanceOperations) {
      finish.push(f.store.begin(operation)!)
      f.store.measure(operation, () => f.advance(100))
    }

    for (const reason of ['INTERACTION', 'API', 'NAVIGATION'] as const)
      f.store.recordProbe(f.store.capture(), reason, 200)
    f.store.recordLongTask(f.report().measurements.elapsedMs - 100, 100)
  }

  let data = f.report().measurements
  expect(data.operations).toHaveLength(80)
  expect(data.inFlight.reduce((total, item) => total + item.count, 0)).toBe(64)
  expect(data.omittedInFlight).toBe(16)
  expect(data.recentSlowSamples).toHaveLength(40)
  expect(new TextEncoder().encode(JSON.stringify(f.report())).length).toBeLessThan(65536)
  for (const end of finish)
    end('ok')
  data = f.report().measurements
  expect(data.inFlight).toEqual([])
  expect(data.omittedInFlight).toBe(0)
  for (const stat of data.operations) {
    expect(stat.p95UpperBoundMs).toBeLessThanOrEqual(stat.maxMs)
    expect(stat.meanMs).toBeLessThanOrEqual(stat.maxMs)
  }
})

it('rejects arbitrary category strings and exposes only declared metadata', () => {
  const f = fixture()
  f.store.setActive(true)
  expect(f.store.begin('private/url' as PerformanceOperation)).toBeNull()
  f.store.recordProbe(f.store.capture(), 'private/url' as ProbeReason, 100)
  const unexpected = f.store.begin('API_READ')!
  f.advance(100)
  unexpected('private outcome' as PerformanceOutcome)
  const extra = { ...build, token: 'private token' }
  const report = f.store.report(extra, 'NONE')!
  expect(JSON.stringify(report)).not.toContain('private')
  expect(f.store.report({ ...build, buildId: 'private build metadata' }, 'NONE')).toBeNull()
  report.measurements.recentSlowSamples[0]!.durationMs = 999
  report.measurements.recentSlowSamples.push({
    elapsedMs: 0,
    screen: 'OTHER',
    kind: 'MARKDOWN',
    durationMs: 100,
    outcome: 'ok',
  })
  expect(f.report().measurements.recentSlowSamples).toEqual([
    {
      elapsedMs: 100,
      screen: 'STARTUP',
      kind: 'API_READ',
      durationMs: 100,
      outcome: 'error',
    },
  ])
})
