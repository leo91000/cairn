import type { UploadEnvironment } from '../src/client-performance-upload'
import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import { ClientPerformance } from '../src/client-performance'
import { performanceUploader } from '../src/client-performance-upload'

afterEach(() => vi.useRealTimers())

function fixture(send = vi.fn<UploadEnvironment['send']>(async () => true)) {
  vi.useFakeTimers()
  const store = new ClientPerformance(() => Date.now(), () => '00000000-0000-4000-8000-000000000001')
  store.setActive(true)
  store.measure('JSON_DECODE', () => {})
  let enabled = true
  const uploader = performanceUploader({
    now: () => Date.now(),
    schedule: (callback, milliseconds) => {
      const timer = setTimeout(callback, milliseconds)
      return () => clearTimeout(timer)
    },
    enabled: () => enabled,
    report: () => store.report({ buildId: 'a'.repeat(64), buildType: 'release', browser: 'FIREFOX' }, 'NONE'),
    send,
  })
  const context = {
    accountId: 'one',
    csrf: 'test-csrf',
    enabled: true,
    foreground: true,
  }
  uploader.update(context)
  return {
    store,
    uploader,
    send,
    context,
    disableElsewhere: () => enabled = false,
  }
}

it('delays first upload, deduplicates accepted revisions and never resets cadence on account changes', async () => {
  const f = fixture()
  await vi.advanceTimersByTimeAsync(9999)
  expect(f.send).not.toHaveBeenCalled()
  await vi.advanceTimersByTimeAsync(1)
  expect(f.send).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(60_000)
  expect(f.send).toHaveBeenCalledTimes(1)
  f.store.measure('MARKDOWN', () => {})
  f.uploader.update({ ...f.context, accountId: 'two' })
  await vi.advanceTimersByTimeAsync(59_999)
  expect(f.send).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  expect(f.send).toHaveBeenCalledTimes(2)
  f.uploader.stop()
  expect(vi.getTimerCount()).toBe(0)
})

it('waits ten seconds after late login and each foreground entry while preserving the minute limit', async () => {
  const f = fixture()
  f.uploader.update({ ...f.context, accountId: '' })
  await vi.advanceTimersByTimeAsync(30_000)
  f.uploader.update(f.context)
  await vi.advanceTimersByTimeAsync(9999)
  expect(f.send).not.toHaveBeenCalled()
  await vi.advanceTimersByTimeAsync(1)
  expect(f.send).toHaveBeenCalledTimes(1)
  f.uploader.update({ ...f.context, foreground: false })
  await vi.advanceTimersByTimeAsync(90_000)
  f.store.measure('MARKDOWN', () => {})
  f.uploader.update(f.context)
  await vi.advanceTimersByTimeAsync(9999)
  expect(f.send).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  expect(f.send).toHaveBeenCalledTimes(2)
  f.uploader.update({ ...f.context, accountId: 'two' })
  f.store.measure('MARKDOWN', () => {})
  await vi.advanceTimersByTimeAsync(59_999)
  expect(f.send).toHaveBeenCalledTimes(2)
  await vi.advanceTimersByTimeAsync(1)
  expect(f.send).toHaveBeenCalledTimes(3)
  f.uploader.stop()
})

it('aborts at five seconds, drops stale success and retries only after the minute', async () => {
  let complete: ((success: boolean) => void) | undefined
  let signal: AbortSignal | undefined
  const send = vi.fn((_report, _csrf, next: AbortSignal) => {
    signal = next
    return new Promise<boolean>((resolve) => {
      complete = resolve
    })
  })
  const f = fixture(send)
  await vi.advanceTimersByTimeAsync(10_000)
  expect(signal?.aborted).toBe(false)
  await vi.advanceTimersByTimeAsync(5000)
  expect(signal?.aborted).toBe(true)
  complete!(true)
  await vi.advanceTimersByTimeAsync(54_999)
  expect(send).toHaveBeenCalledTimes(1)
  await vi.advanceTimersByTimeAsync(1)
  expect(send).toHaveBeenCalledTimes(2)
  f.uploader.stop()
  complete!(false)
  await vi.advanceTimersByTimeAsync(60_000)
  expect(send).toHaveBeenCalledTimes(2)
})

it('cancels background, opt-out and logout uploads and rechecks preferences from other origins', async () => {
  let complete: ((success: boolean) => void) | undefined
  let signal: AbortSignal | undefined
  const send = vi.fn((_report, _csrf, next: AbortSignal) => {
    signal = next
    return new Promise<boolean>((resolve) => {
      complete = resolve
    })
  })
  const f = fixture(send)
  await vi.advanceTimersByTimeAsync(10_000)
  f.uploader.update({ ...f.context, foreground: false })
  expect(signal?.aborted).toBe(true)
  complete!(true)
  await vi.advanceTimersByTimeAsync(60_000)
  expect(send).toHaveBeenCalledTimes(1)
  f.uploader.update({ ...f.context, enabled: false })
  await vi.advanceTimersByTimeAsync(60_000)
  expect(send).toHaveBeenCalledTimes(1)
  f.uploader.update({ ...f.context, accountId: '' })
  await vi.advanceTimersByTimeAsync(60_000)
  expect(send).toHaveBeenCalledTimes(1)
  f.disableElsewhere()
  f.uploader.update(f.context)
  await vi.advanceTimersByTimeAsync(60_000)
  expect(send).toHaveBeenCalledTimes(1)
  f.uploader.stop()
})
