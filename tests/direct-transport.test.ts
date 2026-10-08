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

async function direct() {
  vi.stubGlobal('fetch', vi.fn((url: string) => Promise.resolve(url.endsWith('/authorize')
    ? Response.json({
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
      })
    : Response.json({ marker: 'relay' }, { headers: { 'x-leo-transport': 'relay' } }))))
  const module = await import('../src/api')
  Object.assign(module.state, { authenticated: true, ready: true, csrf: 'csrf' })
  await vi.waitFor(() => expect(Source.all.length).toBe(1))
  Source.all[0].dispatchEvent(new MessageEvent('signal', { data: JSON.stringify({ kind: 'answer', sdp: 'answer' }) }))
  await vi.waitFor(() => expect(module.state.transportRoute).toBe('direct'))
  return { ...module, channel: Peer.all[0].channel }
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
