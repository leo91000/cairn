import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { nextTick } from 'vue'

class Source extends EventTarget {
  static all: Source[] = []
  onerror: (() => void) | null = null
  constructor(readonly url: string) {
    super()
    Source.all.push(this)
  }

  close() {}
}

class Channel extends EventTarget {
  readyState = 'connecting'
  bufferedAmount = 0
  bufferedAmountLowThreshold = 0
  binaryType = ''
  packets: Uint8Array[] = []
  send(packet: ArrayBuffer) {
    this.packets.push(new Uint8Array(packet))
    const data = new Uint8Array(packet)
    if (data.length < 16384 && data.length > 13) {
      try {
        const frame = JSON.parse(new TextDecoder().decode(data.subarray(13)))
        if (frame.method === 'HEAD') {
          queueMicrotask(() => reply(this, {
            type: 'response',
            id: frame.id,
            status: 200,
            headers: [],
            body: '',
          }))
        }
      }
      catch {}
    }
  }

  close() { this.readyState = 'closed' }
}

class Peer extends EventTarget {
  static all: Peer[] = []
  channel = new Channel()
  localDescription = { sdp: `v=0\r\na=fingerprint:sha-256 ${'AA:'.repeat(31)}AA\r\n` }
  iceConnectionState = 'new'
  connectionState = 'new'
  onicecandidate: any
  onconnectionstatechange: any
  constructor() {
    super()
    Peer.all.push(this)
  }

  createDataChannel() { return this.channel }
  async createOffer() { return this.localDescription }
  async setLocalDescription() {}
  async setRemoteDescription() {
    this.channel.readyState = 'open'
    this.channel.dispatchEvent(new Event('open'))
  }

  async addIceCandidate() {}
  setConfiguration() {}
  close() { this.channel.close() }
}

beforeEach(() => {
  vi.resetModules()
  vi.useFakeTimers()
  Source.all = []
  Peer.all = []
  vi.stubGlobal('window', Object.assign(new EventTarget(), { location: { pathname: '/installations/install/', origin: 'https://leo.test' } }))
  vi.stubGlobal('document', Object.assign(new EventTarget(), { getElementById: () => ({ hasAttribute: () => true }), hidden: false }))
  vi.stubGlobal('navigator', { onLine: true })
  vi.stubGlobal('EventSource', Source)
  vi.stubGlobal('RTCPeerConnection', Peer)
})
afterEach(async () => {
  const { state } = await import('../src/api')
  state.authenticated = false
  vi.restoreAllMocks()
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

it('renders through the relay while direct authorization is pending, then uses the authorized channel', async () => {
  let authorize: (value: Response) => void = () => {}
  const fetch = vi.fn((url: string) => url.endsWith('/direct/authorize')
    ? new Promise<Response>(resolve => authorize = resolve)
    : Promise.resolve(Response.json({ marker: 'relay' }, { headers: { 'x-leo-transport': 'relay' } })))
  vi.stubGlobal('fetch', fetch)
  const { api, state } = await import('../src/api')
  state.csrf = 'test-csrf'
  state.authenticated = true
  state.ready = true
  await nextTick()
  expect(await api('/chats')).toEqual({ marker: 'relay' })
  await vi.waitFor(() => expect(fetch.mock.calls.some(([url]) => url.endsWith('/direct/authorize'))).toBe(true))
  authorize(Response.json({
    available: true,
    grant: {
      claims: {
        connection_id: 'connection',
        account_id: 'account',
        role: 'owner',
        expires_at: Date.now() / 1000 + 180,
      },
    },
    iceServers: [],
  }))
  await vi.waitFor(() => expect(Source.all.some(source => source.url.endsWith('/direct/connection/events'))).toBe(true))
  const source = Source.all.find(source => source.url.endsWith('/direct/connection/events'))!
  source.dispatchEvent(new MessageEvent('signal', { data: JSON.stringify({ kind: 'answer', sdp: 'answer' }) }))
  await vi.waitFor(() => expect(state.transportRoute).toBe('direct'))
})

function reply(channel: Channel, frame: object) {
  const body = new TextEncoder().encode(JSON.stringify(frame))
  const packet = new Uint8Array(13 + body.length)
  const header = new DataView(packet.buffer)
  header.setUint8(0, 1)
  header.setUint32(1, 1)
  header.setUint32(5, body.length)
  header.setUint32(9, 0)
  packet.set(body, 13)
  channel.dispatchEvent(new MessageEvent('message', { data: packet.buffer }))
}

async function direct(role: 'owner' | 'member' = 'owner') {
  const expiresAt = Date.now() / 1000 + 180
  vi.stubGlobal('fetch', vi.fn((url: string) => Promise.resolve(url.endsWith('/authorize')
    ? Response.json({
        available: true,
        grant: {
          claims: {
            connection_id: 'connection',
            account_id: 'account',
            role,
            expires_at: expiresAt,
          },
        },
        iceServers: [],
      })
    : Response.json({ marker: 'relay' }, { headers: { 'x-leo-transport': 'relay' } }))))
  const module = await import('../src/api')
  Object.assign(module.state, {
    authenticated: true,
    ready: true,
    csrf: 'csrf',
    installationRole: role,
  })
  await vi.waitFor(() => expect(Source.all.length).toBe(1))
  Source.all[0].dispatchEvent(new MessageEvent('signal', { data: JSON.stringify({ kind: 'answer', sdp: 'answer' }) }))
  await vi.waitFor(() => expect(module.state.transportRoute).toBe('direct'))
  return { ...module, channel: Peer.all[0].channel, expiresAt }
}

it('reads JSON from the actual direct response instead of relaying its payload', async () => {
  const { api, channel } = await direct()
  const response = api('/chats')
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  const request = JSON.parse(new TextDecoder().decode(channel.packets[0].subarray(13)))
  expect(request.path).toBe('/api/chats')
  reply(channel, {
    type: 'response',
    id: request.id,
    status: 200,
    headers: [],
    body: btoa('{"marker":"direct"}'),
  })
  expect(await response).toEqual({ marker: 'direct' })
})

it('retries interrupted reads on relay but leaves other mutations visibly failed', async () => {
  const { api, channel } = await direct()
  const read = api('/chats')
  const write = api('/chats/c', { method: 'DELETE' })
  const failed = expect(write).rejects.toThrow('Connection interrupted')
  await vi.waitFor(() => expect(channel.packets.length).toBe(2))
  channel.dispatchEvent(new Event('close'))
  expect(await read).toEqual({ marker: 'relay' })
  await failed
  expect(vi.mocked(fetch).mock.calls.filter(([url]) => String(url).endsWith('/api/chats/c'))).toHaveLength(0)
})

it('sends an unsent mutation through relay when the channel is closing', async () => {
  const { api, channel, state } = await direct()
  channel.readyState = 'closing'
  expect(await api('/chats/c', { method: 'DELETE' })).toEqual({ marker: 'relay' })
  expect(state.error).toBe('')
})

it('replays a fragmented mutation refused before dispatch through relay without closing direct', async () => {
  const { api, channel, state } = await direct('member')
  const body = JSON.stringify({ name: 'Large mission', prompt: 'x'.repeat(20000) })
  const response = api('/tasks', { method: 'POST', body })
  await vi.waitFor(() => expect(channel.packets.length).toBeGreaterThan(1))
  const first = channel.packets[0]
  const id = JSON.parse(`${new TextDecoder().decode(first.subarray(13)).split(',"body":')[0]}}`).id
  reply(channel, {
    type: 'response',
    id,
    status: 503,
    headers: [['x-leo-direct-rejection', 'reassembly-busy']],
    body: btoa('Direct reassembly busy.'),
  })
  expect(await response).toEqual({ marker: 'relay' })
  expect(fetch).toHaveBeenLastCalledWith('/api/installations/install/api/tasks', expect.objectContaining({ method: 'POST', body }))
  expect(vi.mocked(fetch).mock.calls.filter(([url]) => String(url).endsWith('/api/tasks'))).toHaveLength(1)
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
})

it.each([
  [503, []],
  [503, [['x-leo-direct-rejection', 'unknown']]],
  [500, [['x-leo-direct-rejection', 'reassembly-busy']]],
])('does not replay an application error or an unknown refusal (%s, %j)', async (status, headers) => {
  const { api, channel } = await direct()
  const response = api('/tasks', { method: 'POST', body: '{}' })
  const rejected = expect(response).rejects.toThrow('Direct reassembly busy.')
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  const request = JSON.parse(new TextDecoder().decode(channel.packets[0].subarray(13)))
  reply(channel, {
    type: 'response',
    id: request.id,
    status,
    headers,
    body: btoa('{"error":"Direct reassembly busy."}'),
  })
  await rejected
  expect(vi.mocked(fetch).mock.calls.filter(([url]) => String(url).endsWith('/api/tasks'))).toHaveLength(0)
})

it('uses fallback capacity for unsent mutations and live streams when direct slots are full', async () => {
  const { api, channel, state } = await direct()
  const occupied = Array.from({ length: 32 }, () => api('/chats'))
  await vi.waitFor(() => expect(channel.packets.length).toBe(32))
  expect(await api('/chats/c', { method: 'DELETE' })).toEqual({ marker: 'relay' })
  const { liveConnection } = await import('../src/live-connection')
  const accept = vi.fn()
  const live = liveConnection('/chats/c/stream', accept, vi.fn(), { cursor: 7, history: 'v1' })
  await vi.waitFor(() => expect(Source.all.some(source => source.url.includes('after=7&history=v1'))).toBe(true))
  const stream = Source.all.find(source => source.url.includes('after=7&history=v1'))!
  stream.dispatchEvent(new MessageEvent('batch', {
    lastEventId: '8',
    data: JSON.stringify({
      history: 'v1',
      events: [],
      reset: false,
      more: false,
    }),
  }))
  expect(accept).toHaveBeenCalledTimes(1)
  live.close()
  await vi.advanceTimersByTimeAsync(10000)
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
  channel.dispatchEvent(new Event('close'))
  await Promise.all(occupied)
})

it('expires only a slow request after allowing the server deadline to respond', async () => {
  const { api, channel, state } = await direct()
  const slow = api('/chats/c', { method: 'DELETE' })
  const rejected = expect(slow).rejects.toThrow('Request timed out')
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  await vi.advanceTimersByTimeAsync(30000)
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
  await vi.advanceTimersByTimeAsync(5000)
  await rejected
  expect(state.transportRoute).toBe('direct')
  const next = api('/chats')
  await vi.advanceTimersByTimeAsync(0)
  const frames = channel.packets.map(packet => JSON.parse(new TextDecoder().decode(packet.subarray(13))))
  const request = frames.findLast(frame => frame.method === 'GET')!
  reply(channel, {
    type: 'response',
    id: request.id,
    status: 200,
    headers: [],
    body: btoa('{"marker":"still direct"}'),
  })
  expect(await next).toEqual({ marker: 'still direct' })
})

it('moves live streams between routes using only the accepted cursor and history', async () => {
  const { channel } = await direct()
  const { liveConnection } = await import('../src/live-connection')
  const accept = vi.fn()
  const connection = liveConnection('/chats/c/stream', accept, vi.fn(), { cursor: 7, history: 'v1' })
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  const request = JSON.parse(new TextDecoder().decode(channel.packets[0].subarray(13)))
  expect(request.path).toBe('/api/chats/c/stream?after=7&history=v1&window=1')
  reply(channel, {
    type: 'stream_start',
    id: request.id,
    status: 200,
    headers: [['content-type', 'text/event-stream']],
    body: '',
  })
  await vi.waitFor(() => expect(channel.packets.length).toBe(2))
  reply(channel, { type: 'stream_chunk', id: request.id, body: btoa('event: batch\nid: 8\ndata: {"history":"v1","events":[],"reset":false,"more":false}\n\n') })
  await vi.waitFor(() => expect(accept).toHaveBeenCalledTimes(1))
  channel.dispatchEvent(new Event('close'))
  await vi.waitFor(() => expect(Source.all.some(source => source.url.includes('after=8&history=v1'))).toBe(true))
  connection.close()
})

it('reuses the client message identifier on fallback and accepts an already used identifier', async () => {
  const { api, channel } = await direct()
  const body = JSON.stringify({ id: 'client-message', text: 'once' })
  vi.mocked(fetch).mockImplementation((url, options) => {
    expect(options?.body).toBe(body)
    expect(String(url)).toMatch(/\/messages$/)
    return Promise.resolve(Response.json({ error: 'This message identifier has already been used.' }, { status: 409 }))
  })
  const response = api('/chats/c/messages', { method: 'POST', body })
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  channel.dispatchEvent(new Event('close'))
  expect(await response).toEqual({})
  expect(fetch).toHaveBeenLastCalledWith('/api/installations/install/api/chats/c/messages', expect.objectContaining({ body }))
})

it('fragments large bodies at 16 KiB, waits for bufferedAmount, and abandons a cancelled transfer', async () => {
  const { api, channel } = await direct()
  const signal = new AbortController()
  const response = api('/chats/c/messages', { method: 'POST', body: JSON.stringify({ id: 'large', text: 'x'.repeat(60000) }), signal: signal.signal })
  const rejected = expect(response).rejects.toMatchObject({ name: 'AbortError' })
  // Model SCTP backpressure after the first packet.
  const send = channel.send.bind(channel)
  channel.send = (packet) => {
    send(packet)
    channel.bufferedAmount = 65536
  }

  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  expect(channel.packets[0].length).toBe(16384)
  signal.abort()
  channel.bufferedAmount = 0
  channel.dispatchEvent(new Event('bufferedamountlow'))
  await rejected
  await vi.waitFor(() => expect(channel.packets.length).toBe(2))
  expect([...channel.packets[1]]).toEqual([1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0])
  channel.bufferedAmount = 0
  channel.dispatchEvent(new Event('bufferedamountlow'))
})

it('serves a short read between upload fragments while respecting channel backpressure', async () => {
  const { api, channel, state } = await direct()
  const signal = new AbortController()
  const upload = api('/chats/c/messages', { method: 'POST', body: JSON.stringify({ id: 'large', text: 'x'.repeat(60000) }), signal: signal.signal })
  const rejected = expect(upload).rejects.toMatchObject({ name: 'AbortError' })
  const send = channel.send.bind(channel)
  channel.send = (packet) => {
    send(packet)
    channel.bufferedAmount = 65536
  }

  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  const read = api('/chats')
  await vi.advanceTimersByTimeAsync(0)
  for (let drain = 0; drain < 2; drain++) {
    channel.bufferedAmount = 0
    channel.dispatchEvent(new Event('bufferedamountlow'))
    await vi.advanceTimersByTimeAsync(0)
  }

  const request = channel.packets.filter(packet => packet.length < 16384)
    .map(packet => JSON.parse(new TextDecoder().decode(packet.subarray(13))))
    .find(frame => frame.method === 'GET')
  expect(request?.path).toBe('/api/chats')
  reply(channel, {
    type: 'response',
    id: request.id,
    status: 200,
    headers: [],
    body: btoa('{"marker":"direct"}'),
  })
  expect(await read).toEqual({ marker: 'direct' })
  expect(state.transportRoute).toBe('direct')
  signal.abort()
  channel.bufferedAmount = 0
  channel.dispatchEvent(new Event('bufferedamountlow'))
  await rejected
})

it('starts heartbeat response timing after transmission instead of closing a backpressured peer', async () => {
  const { channel, state } = await direct()
  channel.bufferedAmount = 65536
  await vi.advanceTimersByTimeAsync(15000)
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
  channel.bufferedAmount = 0
  channel.dispatchEvent(new Event('bufferedamountlow'))
  await vi.advanceTimersByTimeAsync(10000)
  expect(state.transportRoute).toBe('direct')
})

it('resets connection backoff after successful reconnection before another transport loss', async () => {
  const { channel, state } = await direct()
  channel.dispatchEvent(new Event('close'))
  await vi.advanceTimersByTimeAsync(30000)
  expect(Peer.all).toHaveLength(2)
  Source.all.at(-1)!.dispatchEvent(new MessageEvent('signal', { data: JSON.stringify({ kind: 'answer', sdp: 'answer' }) }))
  await vi.advanceTimersByTimeAsync(0)
  expect(state.transportRoute).toBe('direct')
  Peer.all[1].channel.dispatchEvent(new Event('close'))
  await vi.advanceTimersByTimeAsync(30000)
  expect(Peer.all).toHaveLength(3)
})

it('renews before expiry and keeps the current channel only after official acceptance', async () => {
  const { state, channel } = await direct()
  vi.mocked(fetch).mockImplementation(() => Promise.resolve(Response.json({
    available: true,
    grant: {
      claims: {
        connection_id: 'connection',
        account_id: 'account',
        role: 'owner',
        expires_at: Date.now() / 1000 + 180,
      },
    },
    iceServers: [],
  })))
  await vi.advanceTimersByTimeAsync(150000)
  expect(fetch).toHaveBeenCalledWith('/api/installations/install/direct/connection/renew', expect.anything())
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
})

it('falls back on an oversized fragment without keeping the interrupted read pending', async () => {
  const { api, channel, state } = await direct()
  const response = api('/chats')
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  const invalid = new ArrayBuffer(14)
  const header = new DataView(invalid)
  header.setUint8(0, 1)
  header.setUint32(1, 1)
  header.setUint32(5, 20_000_000)
  channel.dispatchEvent(new MessageEvent('message', { data: invalid }))
  expect(await response).toEqual({ marker: 'relay' })
  expect(state.transportRoute).toBe('relay')
})

it('stays quietly on relay when Local Network Access is denied', async () => {
  vi.spyOn(Peer.prototype, 'createOffer').mockRejectedValueOnce(new DOMException('Denied', 'NotAllowedError'))
  vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(Response.json({ marker: 'relay' }))))
  const { api, state } = await import('../src/api')
  Object.assign(state, { authenticated: true, ready: true })
  await vi.waitFor(() => expect(Peer.all[0].channel.readyState).toBe('closed'))
  expect(await api('/chats')).toEqual({ marker: 'relay' })
  expect(state.transportRoute).toBe('relay')
  expect(state.error).toBe('')
})

it('starts a fresh authorized ICE negotiation on a network change while continuing on relay', async () => {
  const offer = vi.spyOn(Peer.prototype, 'createOffer')
  const { api, channel, state } = await direct()
  window.dispatchEvent(new Event('online'))
  expect(state.transportRoute).toBe('relay')
  expect(channel.readyState).toBe('closed')
  expect(await api('/chats')).toEqual({ marker: 'relay' })
  await vi.waitFor(() => expect(offer).toHaveBeenLastCalledWith({ iceRestart: true }))
})

it('releases direct capacity after a hidden-tab grace period and reconnects when visible', async () => {
  const { api, channel, state } = await direct()
  Object.defineProperty(document, 'hidden', { value: true, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(29999)
  expect(state.transportRoute).toBe('direct')
  await vi.advanceTimersByTimeAsync(1)
  expect(channel.readyState).toBe('closed')
  expect(state.transportRoute).toBe('relay')
  const attempts = Peer.all.length
  await vi.advanceTimersByTimeAsync(300000)
  expect(Peer.all).toHaveLength(attempts)
  expect(await api('/chats')).toEqual({ marker: 'relay' })
  Object.defineProperty(document, 'hidden', { value: false, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.waitFor(() => expect(Peer.all).toHaveLength(attempts + 1))
})

it.each([
  ['unsupported installation', () => Response.json({ available: false })],
  ['refused authorization', () => Response.json({ error: 'Refused' }, { status: 403 })],
] as const)('stops direct retries for %s until the network changes', async (_, response) => {
  const network = Object.assign(new EventTarget(), {
    type: 'wifi',
    effectiveType: '4g',
    rtt: 50,
    downlink: 10,
  })
  Object.assign(navigator, { connection: network })
  const fetch = vi.fn(() => Promise.resolve(response()))
  vi.stubGlobal('fetch', fetch)
  const { state } = await import('../src/api')
  Object.assign(state, { authenticated: true, ready: true })
  await vi.advanceTimersByTimeAsync(600000)
  expect(fetch).toHaveBeenCalledTimes(1)
  Object.assign(network, { rtt: 100, downlink: 5 })
  network.dispatchEvent(new Event('change'))
  await vi.advanceTimersByTimeAsync(600000)
  expect(fetch).toHaveBeenCalledTimes(1)
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(0)
  expect(fetch).toHaveBeenCalledTimes(1)
  window.dispatchEvent(new Event('online'))
  await vi.advanceTimersByTimeAsync(0)
  expect(fetch).toHaveBeenCalledTimes(2)
})

it('backs off capacity failures exponentially while relay requests remain usable', async () => {
  const network = Object.assign(new EventTarget(), { effectiveType: '4g', rtt: 50, downlink: 10 })
  Object.assign(navigator, { connection: network })
  const fetch = vi.fn((url: string) => Promise.resolve(url.endsWith('/authorize')
    ? Response.json({ error: 'Direct connection capacity reached' }, { status: 503 })
    : Response.json({ marker: 'relay' })))
  vi.stubGlobal('fetch', fetch)
  const { api, state } = await import('../src/api')
  Object.assign(state, { authenticated: true, ready: true })
  await vi.advanceTimersByTimeAsync(0)
  const attempts = () => fetch.mock.calls.filter(([url]) => url.endsWith('/authorize')).length
  expect(attempts()).toBe(1)
  await vi.advanceTimersByTimeAsync(30000)
  expect(attempts()).toBe(2)
  Object.assign(network, { rtt: 100, downlink: 5 })
  network.dispatchEvent(new Event('change'))
  await vi.advanceTimersByTimeAsync(59999)
  expect(attempts()).toBe(2)
  await vi.advanceTimersByTimeAsync(1)
  expect(attempts()).toBe(3)
  await vi.advanceTimersByTimeAsync(119999)
  expect(attempts()).toBe(3)
  expect(await api('/chats')).toEqual({ marker: 'relay' })
})

it('keeps the capacity backoff deadline when a hidden tab becomes visible', async () => {
  vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(Response.json({ error: 'Direct connection capacity reached' }, { status: 503 }))))
  const { state } = await import('../src/api')
  Object.assign(state, { authenticated: true, ready: true })
  await vi.advanceTimersByTimeAsync(30000)
  expect(fetch).toHaveBeenCalledTimes(2)
  Object.defineProperty(document, 'hidden', { value: true, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(40000)
  Object.defineProperty(document, 'hidden', { value: false, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(19999)
  expect(fetch).toHaveBeenCalledTimes(2)
  await vi.advanceTimersByTimeAsync(1)
  expect(fetch).toHaveBeenCalledTimes(3)
  await vi.advanceTimersByTimeAsync(119999)
  expect(fetch).toHaveBeenCalledTimes(3)
  await vi.advanceTimersByTimeAsync(1)
  expect(fetch).toHaveBeenCalledTimes(4)
})

it('resumes an elapsed backoff once visible without negotiating in the hidden tab', async () => {
  vi.stubGlobal('fetch', vi.fn(() => Promise.resolve(Response.json({ error: 'Direct connection capacity reached' }, { status: 503 }))))
  const { state } = await import('../src/api')
  Object.assign(state, { authenticated: true, ready: true })
  await vi.advanceTimersByTimeAsync(0)
  Object.defineProperty(document, 'hidden', { value: true, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(60000)
  expect(fetch).toHaveBeenCalledTimes(1)
  Object.defineProperty(document, 'hidden', { value: false, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(0)
  expect(fetch).toHaveBeenCalledTimes(2)
  await vi.advanceTimersByTimeAsync(59999)
  expect(fetch).toHaveBeenCalledTimes(2)
  await vi.advanceTimersByTimeAsync(1)
  expect(fetch).toHaveBeenCalledTimes(3)
})

it('waits for the retry deadline after a direct failure in a hidden tab', async () => {
  const { channel } = await direct()
  Object.defineProperty(document, 'hidden', { value: true, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  channel.dispatchEvent(new Event('close'))
  await vi.advanceTimersByTimeAsync(10000)
  Object.defineProperty(document, 'hidden', { value: false, configurable: true })
  document.dispatchEvent(new Event('visibilitychange'))
  await vi.advanceTimersByTimeAsync(19999)
  expect(Peer.all).toHaveLength(1)
  await vi.advanceTimersByTimeAsync(1)
  expect(Peer.all).toHaveLength(2)
})

it('renegotiates after the existing availability check observes installation recovery', async () => {
  const { state, channel } = await direct()
  Object.assign(state, { installationOnline: false })
  await vi.advanceTimersByTimeAsync(0)
  expect(state.transportRoute).toBe('relay')
  expect(channel.readyState).toBe('closed')
  const peers = Peer.all.length
  await vi.advanceTimersByTimeAsync(300000)
  expect(Peer.all).toHaveLength(peers)
  Object.assign(state, { installationOnline: true })
  await vi.advanceTimersByTimeAsync(0)
  expect(Peer.all).toHaveLength(peers + 1)
})

it('ignores mDNS candidates, signals numeric candidates and keeps binary resources on the relay', async () => {
  const { api, apiResourceUrl } = await direct()
  const candidate = 'candidate:1 1 udp 2122260223 browser.local 1234 typ host'
  Peer.all[0].onicecandidate({ candidate: { candidate, sdpMid: '0', sdpMLineIndex: 0 } })
  const signalCalls = () => vi.mocked(fetch).mock.calls.filter(([url, options]) => String(url).endsWith('/signal') && JSON.parse(String(options?.body)).kind === 'candidate')
  await vi.advanceTimersByTimeAsync(0)
  expect(signalCalls()).toHaveLength(0)
  const addCandidate = vi.spyOn(Peer.prototype, 'addIceCandidate')
  Source.all[0].dispatchEvent(new MessageEvent('signal', {
    data: JSON.stringify({
      kind: 'candidate',
      candidate,
      sdp_mid: '0',
      sdp_m_line_index: 0,
    }),
  }))
  await vi.advanceTimersByTimeAsync(0)
  expect(addCandidate).not.toHaveBeenCalled()

  const numeric = candidate.replace('browser.local', '192.168.1.10')
  Peer.all[0].onicecandidate({ candidate: { candidate: numeric, sdpMid: '0', sdpMLineIndex: 0 } })
  await vi.waitFor(() => expect(signalCalls()).toHaveLength(1))
  expect(JSON.parse(String(signalCalls()[0][1]?.body)).candidate).toBe(numeric)
  expect(apiResourceUrl('/api/chats/c/attachments/file')).toBe('/api/installations/install/api/chats/c/attachments/file')
  await api('/chats/c/attachments', { method: 'POST', body: new Blob(['binary']) })
  expect(fetch).toHaveBeenLastCalledWith('/api/installations/install/api/chats/c/attachments', expect.objectContaining({ body: expect.any(Blob) }))
})

it('filters special ICE destinations from offers and trickle on both directions', async () => {
  const addresses = ['127.0.0.1', '169.254.1.2', '224.1.2.3', '255.255.255.255', '0.0.0.0', '::', '::1', 'fe80::1', 'ff02::1', '::ffff:127.0.0.1', 'browser.local']
  const candidates = addresses.map(address => `candidate:1 1 udp 2122260223 ${address} 1234 typ host`)
  const numeric = 'candidate:1 1 udp 2122260223 192.168.1.10 1234 typ host'
  vi.spyOn(Peer.prototype, 'createOffer').mockImplementation(async function (this: Peer) {
    return { sdp: this.localDescription.sdp + [...candidates, numeric].map(candidate => `a=${candidate}\r\n`).join('') }
  })
  const { state } = await direct()
  const offered = vi.mocked(fetch).mock.calls.find(([url]) => String(url).endsWith('/signal'))!
  expect(JSON.parse(String(offered[1]?.body)).sdp).toContain(numeric)
  for (const candidate of candidates)
    expect(JSON.parse(String(offered[1]?.body)).sdp).not.toContain(candidate)
  const addCandidate = vi.spyOn(Peer.prototype, 'addIceCandidate')
  for (const candidate of candidates) {
    Peer.all[0].onicecandidate({ candidate: { candidate, sdpMid: '0', sdpMLineIndex: 0 } })
    Source.all[0].dispatchEvent(new MessageEvent('signal', {
      data: JSON.stringify({
        kind: 'candidate',
        candidate,
        sdp_mid: '0',
        sdp_m_line_index: 0,
      }),
    }))
  }

  await vi.advanceTimersByTimeAsync(0)
  expect(addCandidate).not.toHaveBeenCalled()
  expect(vi.mocked(fetch).mock.calls.filter(([url, options]) => String(url).endsWith('/signal') && JSON.parse(String(options?.body)).kind === 'candidate')).toHaveLength(0)
  expect(state.transportRoute).toBe('direct')
})

it('continues candidate signaling after one signal is refused', async () => {
  const { state } = await direct()
  vi.mocked(fetch).mockResolvedValueOnce(Response.json({ error: 'Installation refused direct signal' }, { status: 429 }))
  const candidate = (address: string) => ({ candidate: `candidate:1 1 udp 2122260223 ${address} 1234 typ host`, sdpMid: '0', sdpMLineIndex: 0 })
  Peer.all[0].onicecandidate({ candidate: candidate('192.168.1.10') })
  Peer.all[0].onicecandidate({ candidate: candidate('192.168.1.11') })
  await vi.advanceTimersByTimeAsync(0)
  expect(state.transportRoute).toBe('direct')
  const signaled = vi.mocked(fetch).mock.calls.filter(([url, options]) => String(url).endsWith('/signal') && JSON.parse(String(options?.body)).kind === 'candidate')
  expect(signaled).toHaveLength(2)
})

it('keeps a working direct peer when only network estimates change', async () => {
  const network = Object.assign(new EventTarget(), {
    type: 'wifi',
    effectiveType: '4g',
    rtt: 50,
    downlink: 10,
  })
  Object.assign(navigator, { connection: network })
  const { api, channel, state } = await direct()
  Object.assign(network, { rtt: 100, downlink: 5 })
  network.dispatchEvent(new Event('change'))
  await vi.advanceTimersByTimeAsync(1000)
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
  expect(Peer.all).toHaveLength(1)
  const read = api('/chats')
  await vi.advanceTimersByTimeAsync(0)
  const request = JSON.parse(new TextDecoder().decode(channel.packets.at(-1)!.subarray(13)))
  reply(channel, {
    type: 'response',
    id: request.id,
    status: 200,
    headers: [],
    body: btoa('{"marker":"direct"}'),
  })
  expect(await read).toEqual({ marker: 'direct' })
})

it.each(['type', 'effectiveType'] as const)('debounces actual network %s changes before restarting ICE', async (field) => {
  const network = Object.assign(new EventTarget(), { type: 'wifi', effectiveType: '4g' })
  Object.assign(navigator, { connection: network })
  const { state } = await direct()
  const peers = Peer.all.length
  network[field] = field === 'type' ? 'cellular' : '3g'
  for (let i = 0; i < 10; i++)
    network.dispatchEvent(new Event('change'))
  await vi.advanceTimersByTimeAsync(999)
  expect(Peer.all).toHaveLength(peers)
  expect(state.transportRoute).toBe('direct')
  await vi.advanceTimersByTimeAsync(1)
  expect(Peer.all).toHaveLength(peers + 1)
})

it('ignores estimate notifications when network identity fields are unavailable', async () => {
  const network = new EventTarget()
  Object.assign(navigator, { connection: network })
  const { channel, state } = await direct()
  network.dispatchEvent(new Event('change'))
  await vi.advanceTimersByTimeAsync(1000)
  expect(state.transportRoute).toBe('direct')
  expect(channel.readyState).toBe('open')
  expect(Peer.all).toHaveLength(1)
})

it('reassembles fragmented UTF-8 responses and discards an abandoned partial response', async () => {
  const { api, channel } = await direct()
  const response = api('/chats')
  await vi.waitFor(() => expect(channel.packets.length).toBe(1))
  const request = JSON.parse(new TextDecoder().decode(channel.packets[0].subarray(13)))
  const text = 'é'.repeat(20000)
  const body = new TextEncoder().encode(JSON.stringify({ marker: text }))
  let encoded = ''
  for (const byte of body)
    encoded += String.fromCharCode(byte)
  const wire = new TextEncoder().encode(JSON.stringify({
    type: 'response',
    id: request.id,
    status: 200,
    headers: [],
    body: btoa(encoded),
  }))

  function packet(id: number, total: number, offset: number, payload: Uint8Array) {
    const output = new Uint8Array(13 + payload.length)
    const header = new DataView(output.buffer)
    header.setUint8(0, 1)
    header.setUint32(1, id)
    header.setUint32(5, total)
    header.setUint32(9, offset)
    output.set(payload, 13)
    channel.dispatchEvent(new MessageEvent('message', { data: output.buffer }))
  }

  packet(9, wire.length, 0, wire.subarray(0, 100))
  packet(9, 0, 0, new Uint8Array())
  for (let offset = 0; offset < wire.length; offset += 16371)
    packet(9, wire.length, offset, wire.subarray(offset, offset + 16371))
  expect(await response).toEqual({ marker: text })
})

it('closes immediately when renewal is refused and ignores late signals from the old lease', async () => {
  const { state, channel } = await direct()
  const source = Source.all[0]
  vi.mocked(fetch).mockImplementation(() => Promise.resolve(Response.json({ error: 'Session expired' }, { status: 401 })))
  await vi.advanceTimersByTimeAsync(150000)
  expect(state.transportRoute).toBe('relay')
  expect(state.authenticated).toBe(false)
  expect(channel.readyState).toBe('closed')
  source.dispatchEvent(new MessageEvent('signal', { data: JSON.stringify({ kind: 'answer', sdp: 'stale' }) }))
  expect(state.transportRoute).toBe('relay')
})

it('keeps a session-capped renewal until expiry without issuing one-second renewals', async () => {
  const { state, expiresAt: expiry } = await direct()
  vi.mocked(fetch).mockImplementation(() => Promise.resolve(Response.json({
    available: true,
    grant: {
      claims: {
        connection_id: 'connection',
        account_id: 'account',
        role: 'owner',
        expires_at: expiry,
      },
    },
    iceServers: [],
  })))
  await vi.advanceTimersByTimeAsync(expiry * 1000 - Date.now() - 1)
  expect(state.transportRoute).toBe('direct')
  expect(vi.mocked(fetch).mock.calls.filter(([url]) => String(url).endsWith('/renew'))).toHaveLength(1)
  await vi.advanceTimersByTimeAsync(1)
  expect(state.transportRoute).toBe('relay')
})
