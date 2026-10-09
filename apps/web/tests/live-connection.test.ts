import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { liveConnection } from '../src/live-connection'

class Source extends EventTarget {
  static instances: Source[] = []
  closed = false
  onerror?: () => void
  constructor(readonly url: string) {
    super()
    Source.instances.push(this)
  }

  close() { this.closed = true }
  batch(id: string, data = '{"events":[],"reset":false,"more":false}') {
    this.dispatchEvent(new MessageEvent('batch', { lastEventId: id, data }))
  }
}

beforeEach(() => {
  vi.useFakeTimers()
  Source.instances = []
  vi.stubGlobal('EventSource', Source)
  vi.stubGlobal('window', new EventTarget())
  vi.stubGlobal('document', Object.assign(new EventTarget(), { hidden: false }))
  vi.stubGlobal('navigator', { onLine: true })
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

it('resumes only accepted batches and ignores callbacks from a replaced connection', () => {
  const accept = vi.fn()
  const status = vi.fn()
  const connection = liveConnection('/runs/r/stream', accept, status)
  const original = Source.instances[0]
  original.batch('9')
  original.batch('10', 'broken JSON')
  expect(original.closed).toBe(true)
  vi.advanceTimersByTime(500)
  expect(Source.instances[1].url).toBe('/api/runs/r/stream?after=9&window=1')
  original.batch('11')
  expect(accept).toHaveBeenCalledTimes(1)
  Source.instances[1].batch('10')
  expect(status).toHaveBeenLastCalledWith('live')
  connection.close()
  original.dispatchEvent(new MessageEvent('ping', { data: '{}' }))
  expect(vi.getTimerCount()).toBe(0)
  window.dispatchEvent(new Event('online'))
  document.dispatchEvent(new Event('visibilitychange'))
  vi.advanceTimersByTime(60000)
  expect(Source.instances).toHaveLength(2)
})

it('recovers silent connections, keeps idle healthy streams open, and resumes after offline', () => {
  const status = vi.fn()
  const connection = liveConnection('/chats/c/stream', vi.fn(), status)
  for (let n = 0; n < 6; n++) {
    vi.advanceTimersByTime(10000)
    Source.instances[0].dispatchEvent(new MessageEvent('ping', { data: '{}' }))
  }

  expect(Source.instances).toHaveLength(1)
  vi.advanceTimersByTime(45500)
  expect(Source.instances).toHaveLength(2)
  Object.defineProperty(navigator, 'onLine', { value: false, configurable: true })
  window.dispatchEvent(new Event('offline'))
  vi.advanceTimersByTime(60000)
  expect(Source.instances).toHaveLength(2)
  expect(status).toHaveBeenLastCalledWith('offline')
  Object.defineProperty(navigator, 'onLine', { value: true })
  window.dispatchEvent(new Event('online'))
  expect(Source.instances).toHaveLength(3)
  connection.close()
})

it('backs off repeated failures and does not advance after a consumer failure', () => {
  const connection = liveConnection('/chats/stream', () => {
    throw new Error('not applied')
  }, vi.fn())
  Source.instances[0].batch('50')
  vi.advanceTimersByTime(500)
  expect(Source.instances[1].url).toBe('/api/chats/stream?after=0')
  Source.instances[1].onerror?.()
  vi.advanceTimersByTime(999)
  expect(Source.instances).toHaveLength(2)
  vi.advanceTimersByTime(1)
  expect(Source.instances).toHaveLength(3)
  connection.close()
})

it('validates cached history and restarts safely when the server no longer supports revisions', () => {
  const accept = vi.fn()
  const connection = liveConnection('/chats/c/stream', accept, vi.fn(), { cursor: 42, history: 'v1:r:1' })
  expect(Source.instances[0].url).toBe('/api/chats/c/stream?after=42&history=v1%3Ar%3A1&window=1')
  Source.instances[0].batch('43', '{"events":[],"reset":false,"more":false,"history":"v1:r:1"}')
  expect(accept).toHaveBeenCalledTimes(1)
  connection.reconnect()
  expect(Source.instances[1].url).toContain('after=43&history=')
  Source.instances[1].batch('44')
  expect(accept).toHaveBeenCalledTimes(1)
  vi.advanceTimersByTime(500)
  expect(Source.instances[2].url).toBe('/api/chats/c/stream?after=0&window=1')
  connection.close()
})

it('replays from zero with a reset when a cached consumer baseline is invalid', () => {
  const accept = vi.fn().mockImplementationOnce(() => {
    throw new Error('invalid baseline')
  })
  const connection = liveConnection('/chats/c/stream', accept, vi.fn(), { cursor: 42, history: 'v1:r:1' })
  Source.instances[0].batch('43', '{"events":[],"reset":false,"more":false,"history":"v1:r:1"}')
  vi.advanceTimersByTime(500)
  expect(Source.instances[1].url).toBe('/api/chats/c/stream?after=0&window=1')
  Source.instances[1].batch('10', '{"events":[],"reset":false,"more":true,"history":"v1:r:1"}')
  expect(accept.mock.calls[1][0].reset).toBe(true)
  connection.close()
})
