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
  send(packet: ArrayBuffer) { this.packets.push(new Uint8Array(packet)) }
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
