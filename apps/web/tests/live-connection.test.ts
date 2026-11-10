import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { state } from '../src/api'
import { liveConnection } from '../src/live-connection'

class Source extends EventTarget {
  static instances: Source[] = []
  closed = false
  private body?: ReadableStreamDefaultController<Uint8Array>
  onerror?: () => void
  constructor(readonly url: string) {
    super()
    Source.instances.push(this)
  }

  close() { this.closed = true }
  response(signal: AbortSignal) {
    signal.addEventListener('abort', () => this.close())
    this.onerror = () => this.body?.error(new TypeError('Connection lost'))
    return new Response(new ReadableStream<Uint8Array>({
      start: controller => this.body = controller,
    }), { headers: { 'content-type': 'text/event-stream' } })
  }

  async batch(id: string, data = '{"events":[],"entries":[],"reset":false,"more":false,"history":"v2:r:1"}') {
    if (!this.closed) {
      if (this.body)
        this.body.enqueue(new TextEncoder().encode(`event: batch\nid: ${id}\ndata: ${data}\n\n`))
      else
        this.dispatchEvent(new MessageEvent('batch', { lastEventId: id, data }))
    }

    await vi.advanceTimersByTimeAsync(0)
  }
}

beforeEach(() => {
  vi.useFakeTimers()
  state.redirecting = false
  state.protocolError = ''
  Source.instances = []
  vi.stubGlobal('EventSource', Source)
  vi.stubGlobal('fetch', vi.fn(async (url: string, options: RequestInit) => new Source(url).response(options.signal!)))
  vi.stubGlobal('window', new EventTarget())
  vi.stubGlobal('document', Object.assign(new EventTarget(), { hidden: false }))
  vi.stubGlobal('navigator', { onLine: true })
})
afterEach(() => {
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

it('resumes only accepted batches and ignores callbacks from a replaced connection', async () => {
  const accept = vi.fn()
  const status = vi.fn()
  const connection = liveConnection('/runs/r/stream', accept, status)
  const original = Source.instances[0]
  await original.batch('9')
  await original.batch('10', 'broken JSON')
  expect(original.closed).toBe(true)
  await vi.advanceTimersByTimeAsync(500)
  expect(Source.instances[1].url).toBe('/api/runs/r/stream?after=9&history=v2%3Ar%3A1&window=1&timeline=1')
  await original.batch('11')
  expect(accept).toHaveBeenCalledTimes(1)
  await Source.instances[1].batch('10')
  expect(status).toHaveBeenLastCalledWith('live')
  connection.close()
  original.dispatchEvent(new MessageEvent('ping', { data: '{}' }))
  expect(vi.getTimerCount()).toBe(0)
  window.dispatchEvent(new Event('online'))
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(60000)
  expect(Source.instances).toHaveLength(2)
})

it('does not advance the cursor while an oversized card is still loading', async () => {
  let complete: (() => void) | undefined
  const accept = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => {
    complete = resolve
  }))
  const connection = liveConnection('/runs/r/stream', accept, vi.fn(), { cursor: 7, history: 'v2:r:1' })
  const first = Source.instances[0]
  await first.batch('9', '{"events":[],"entries":[],"reset":false,"more":false,"history":"v2:r:1"}')
  first.onerror?.()
  await vi.advanceTimersByTimeAsync(500)
  expect(Source.instances[1].url).toContain('after=7')
  complete?.()
  await Promise.resolve()
  await Source.instances[1].batch('10', '{"events":[],"entries":[],"reset":false,"more":false,"history":"v2:r:1"}')
  expect(accept).toHaveBeenCalledTimes(2)
  connection.close()
})

it('accepts later frames only after an oversized card completes', async () => {
  let complete: (() => void) | undefined
  const accept = vi.fn().mockImplementationOnce(() => new Promise<void>((resolve) => {
    complete = resolve
  }))
  const connection = liveConnection('/runs/r/stream', accept, vi.fn())
  const source = Source.instances[0]
  await source.batch('1')
  await source.batch('2')
  expect(accept).toHaveBeenCalledTimes(1)
  complete?.()
  await Promise.resolve()
  await Promise.resolve()
  await Promise.resolve()
  expect(accept).toHaveBeenCalledTimes(2)
  expect(accept.mock.calls.map(call => call[1])).toEqual([1, 2])
  connection.close()
})

it('announces every opening before creating a source so the caller can free its stream slot', async () => {
  const openedAfter: number[] = []
  const connection = liveConnection('/chats/c/stream', vi.fn(), (_status, opening) => {
    if (opening)
      openedAfter.push(Source.instances.length)
  })
  expect(openedAfter).toEqual([0])
  Source.instances[0].onerror?.()
  await vi.advanceTimersByTimeAsync(500)
  expect(openedAfter).toEqual([0, 1])
  expect(Source.instances).toHaveLength(2)
  connection.close()
})

it('recovers silent connections, keeps idle healthy streams open, and resumes after offline', async () => {
  const status = vi.fn()
  const connection = liveConnection('/chats/c/stream', vi.fn(), status)
  for (let n = 0; n < 6; n++) {
    await vi.advanceTimersByTimeAsync(10000)
    await Source.instances[0].batch('1')
  }

  expect(Source.instances).toHaveLength(1)
  await vi.advanceTimersByTimeAsync(45500)
  expect(Source.instances).toHaveLength(2)
  Object.defineProperty(navigator, 'onLine', { value: false, configurable: true })
  window.dispatchEvent(new Event('offline'))
  await vi.advanceTimersByTimeAsync(60000)
  expect(Source.instances).toHaveLength(2)
  expect(status).toHaveBeenLastCalledWith('offline')
  Object.defineProperty(navigator, 'onLine', { value: true })
  window.dispatchEvent(new Event('online'))
  expect(Source.instances).toHaveLength(3)
  connection.close()
})

it('backs off repeated failures and does not advance after a consumer failure', async () => {
  const connection = liveConnection('/chats/stream', async () => {
    throw new Error('not applied')
  }, vi.fn())
  await Source.instances[0].batch('50')
  await vi.advanceTimersByTimeAsync(500)
  expect(Source.instances[1].url).toBe('/api/chats/stream?after=0')
  Source.instances[1].onerror?.()
  await vi.advanceTimersByTimeAsync(999)
  expect(Source.instances).toHaveLength(2)
  await vi.advanceTimersByTimeAsync(1)
  expect(Source.instances).toHaveLength(3)
  connection.close()
})

it('preserves cached history when the server no longer supports revisions', async () => {
  const accept = vi.fn()
  const status = vi.fn()
  const connection = liveConnection('/chats/c/stream', accept, status, { cursor: 42, history: 'v2:r:1' })
  await Source.instances[0].batch('43')
  expect(accept).toHaveBeenCalledTimes(1)
  connection.reconnect()
  expect(Source.instances[1].url).toContain('after=43&history=')
  await Source.instances[1].batch('44', '{"events":[],"entries":[],"reset":false,"more":false}')
  expect(accept).toHaveBeenCalledTimes(1)
  expect(status).toHaveBeenLastCalledWith('update-required')
  await vi.advanceTimersByTimeAsync(60000)
  expect(Source.instances).toHaveLength(2)
  connection.close()
})

it('offers the recent window only when a viewer following the latest output resumes', async () => {
  let follows = true
  const connection = liveConnection('/chats/c/stream', vi.fn(), vi.fn(), { cursor: 42, history: 'v2:r:1' }, () => follows)
  expect(Source.instances[0].url).toBe('/api/chats/c/stream?after=42&history=v2%3Ar%3A1&window=1&timeline=1&recent=1')
  // A reader of older messages keeps the replay that preserves their position.
  follows = false
  connection.reconnect()
  expect(Source.instances[1].url).toBe('/api/chats/c/stream?after=42&history=v2%3Ar%3A1&window=1&timeline=1')
  connection.close()
  // A fresh stream already opens on the recent window; the conversation list has none.
  const fresh = liveConnection('/runs/r/stream', vi.fn(), vi.fn(), undefined, () => true)
  expect(Source.instances[2].url).toBe('/api/runs/r/stream?after=0&window=1&timeline=1')
  fresh.close()
  const list = liveConnection('/chats/stream', vi.fn(), vi.fn(), { cursor: 7, history: 'v1:list:1' }, () => true)
  expect(Source.instances[3].url).toBe('/api/chats/stream?after=7&history=v1%3Alist%3A1')
  list.close()
})

it('replays from zero with a reset when a cached consumer baseline is invalid', async () => {
  const accept = vi.fn().mockImplementationOnce(() => {
    throw new Error('invalid baseline')
  })
  const connection = liveConnection('/chats/c/stream', accept, vi.fn(), { cursor: 42, history: 'v2:r:1' })
  await Source.instances[0].batch('43', '{"events":[],"entries":[],"reset":false,"more":false,"history":"v2:r:1"}')
  await vi.advanceTimersByTimeAsync(500)
  expect(Source.instances[1].url).toBe('/api/chats/c/stream?after=0&window=1&timeline=1')
  await Source.instances[1].batch('10', '{"events":[],"entries":[],"reset":false,"more":true,"history":"v2:r:1"}')
  expect(accept.mock.calls[1][0].reset).toBe(true)
  connection.close()
})

it('suspends live streams during navigation and resumes even when direct remains unavailable', async () => {
  const connection = liveConnection('/chats/stream', vi.fn(), vi.fn())
  expect(Source.instances).toHaveLength(1)
  state.redirecting = true
  expect(Source.instances[0].closed).toBe(true)

  window.dispatchEvent(new Event('cairn-transport-change'))
  window.dispatchEvent(new Event('online'))
  window.dispatchEvent(new Event('pageshow'))
  await vi.advanceTimersByTimeAsync(60000)
  expect(Source.instances).toHaveLength(1)

  state.redirecting = false
  state.protocolError = ''
  expect(Source.instances).toHaveLength(2)
  connection.close()
  state.redirecting = true
  state.redirecting = false
  state.protocolError = ''
  expect(Source.instances).toHaveLength(2)
})

it('refuses an events-only conversation before acceptance and stops reconnecting', async () => {
  const accept = vi.fn()
  const status = vi.fn()
  const connection = liveConnection('/chats/c/stream', accept, status, { cursor: 12, history: 'v2:r:1' })
  const source = Source.instances[0]
  await source.batch('20', JSON.stringify({
    events: [],
    entries: [],
    reset: false,
    more: false,
    history: 'v1:r:1',
  }))
  expect(accept).not.toHaveBeenCalled()
  expect(source.closed).toBe(true)
  expect(status).toHaveBeenLastCalledWith('update-required')
  expect(state.protocolError).toMatch(/update required/i)
  window.dispatchEvent(new Event('online'))
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(120000)
  expect(Source.instances).toHaveLength(1)
  connection.close()
})

it.each([
  { events: [], reset: false, more: false },
  {
    events: [],
    entries: [],
    reset: false,
    more: false,
    history: 'v3:r:1',
  },
  {
    events: [],
    reset: false,
    more: false,
    history: 'v2:r:1',
  },
])('preserves the accepted baseline and refuses unsupported conversation representations: %j', async (batch) => {
  const accept = vi.fn()
  const status = vi.fn()
  const connection = liveConnection('/chats/c/stream', accept, status, { cursor: 12, history: 'v2:r:1' })
  await Source.instances[0].batch('20', JSON.stringify(batch))
  expect(accept).not.toHaveBeenCalled()
  expect(status).toHaveBeenLastCalledWith('update-required')
  connection.reconnect()
  window.dispatchEvent(new Event('offline'))
  window.dispatchEvent(new Event('online'))
  await vi.advanceTimersByTimeAsync(120000)
  expect(Source.instances).toHaveLength(1)
  expect(status).toHaveBeenLastCalledWith('update-required')
  connection.close()
})
