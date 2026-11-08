import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'

const fixture = new URL('../../tests/fixtures/timeline-client-v0.74.5/', import.meta.url)
const provenance = JSON.parse(readFileSync(new URL('provenance.json', fixture), 'utf8'))
let stop

function receive(channel, frame) {
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

class Channel extends EventTarget {
  readyState = 'connecting'
  bufferedAmount = 0
  packets = []
  send(packet) {
    this.packets.push(packet)
    const frame = JSON.parse(new TextDecoder().decode(new Uint8Array(packet).subarray(13)))
    if (frame.method === 'HEAD') {
      queueMicrotask(() => receive(this, {
        type: 'response',
        id: frame.id,
        status: 200,
        headers: [],
        body: '',
      }))
    }
  }

  close() { this.readyState = 'closed' }
}

class Source extends EventTarget {
  static all = []
  constructor(url) {
    super()
    this.url = url
    Source.all.push(this)
  }

  close() {}
}

class Peer {
  static all = []
  channel = new Channel()
  configurations = []
  connectionState = 'new'
  constructor() { Peer.all.push(this) }
  createDataChannel() { return this.channel }
  async createOffer() { return { sdp: `v=0\r\na=fingerprint:sha-256 ${'AA:'.repeat(31)}AA\r\n` } }
  async setLocalDescription() {}
  async setRemoteDescription() {
    this.channel.readyState = 'open'
    this.channel.dispatchEvent(new Event('open'))
  }

  async addIceCandidate() {}
  setConfiguration(value) { this.configurations.push(value) }
  close() { this.channel.close() }
}

function archivedTransport(fetch) {
  const modules = new Map()
  const environment = {
    EventTarget,
    Event,
    MessageEvent,
    AbortController,
    Response,
    TextEncoder,
    TextDecoder,
    ArrayBuffer,
    DataView,
    Uint8Array,
    DOMException,
    Date,
    atob,
    btoa,
    crypto: globalThis.crypto,
    Headers,
    Request,
    ReadableStream,
    DecompressionStream,
    URL,
    setTimeout,
    clearTimeout,
    setInterval,
    clearInterval,
    queueMicrotask,
    navigator: { onLine: true },
    document: Object.assign(new EventTarget(), { hidden: false }),
    window: new EventTarget(),
    RTCPeerConnection: Peer,
    EventSource: Source,
    fetch,
  }
  const load = (name) => {
    if (name === './transport-observation')
      return { observeTransport() {} }
    if (!['./direct-channel', './direct-source', './installation-transport'].includes(name))
      throw new Error('Unexpected archived dependency')
    if (modules.has(name))
      return modules.get(name)
    const key = name.slice(2)
    const source = readFileSync(new URL(`${key}.ts.fixture`, fixture), 'utf8')
    if (createHash('sha256').update(source).digest('hex') !== provenance.sha256[key])
      throw new Error('Archived client source identity changed')
    const compiled = ts.transpileModule(source, {
      compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
    }).outputText
    const exports = {}
    modules.set(name, exports)
    vm.runInNewContext(compiled, { ...environment, exports, require: load }, { filename: `${provenance.commit}/${key}.ts` })
    return exports
  }

  const { InstallationTransport } = load('./installation-transport')
  const context = {
    installationId: 'install',
    csrf: 'csrf',
    authenticated: true,
    ready: true,
    transportRoute: 'relay',
    protocolError: '',
  }
  const transport = new InstallationTransport(context)
  stop = () => transport.stop()
  transport.start()
  return { transport, context }
}

beforeEach(() => {
  vi.useFakeTimers()
  Source.all = []
  Peer.all = []
})
afterEach(() => {
  stop?.()
  stop = undefined
  vi.useRealTimers()
})

it('the distributed client shows an authorization refusal before ICE or any UDP dependency', async () => {
  const fetch = vi.fn(async (_url, options) => {
    expect(JSON.parse(options.body)).toEqual({ fingerprint: `sha-256 ${'AA:'.repeat(31)}AA`, version: 4 })
    return Response.json({ error: 'Timeline protocol mismatch. Update required: update the app.' }, { status: 426 })
  })
  const { context, transport } = archivedTransport(fetch)
  await vi.waitFor(() => expect(context.protocolError).toContain('update the app'))
  expect(context.transportRoute).toBe('relay')
  expect(Peer.all[0].configurations).toHaveLength(0)
  expect(() => transport.source('/chats/c/stream', '/api/chats/c/stream')).toThrow('Update required')
  await vi.advanceTimersByTimeAsync(300000)
  expect(fetch).toHaveBeenCalledOnce()
})

async function connected(fetch) {
  const value = archivedTransport(fetch)
  await vi.waitFor(() => expect(Source.all).toHaveLength(1))
  Source.all[0].dispatchEvent(new MessageEvent('signal', { data: JSON.stringify({ kind: 'answer', sdp: 'v=0\r\n' }) }))
  await vi.waitFor(() => expect(value.context.transportRoute).toBe('direct'))
  return { ...value, channel: Peer.all[0].channel }
}

function grant() {
  return Response.json({
    available: true,
    grant: {
      claims: {
        connection_id: 'connection',
        account_id: 'account',
        role: 'owner',
        expires_at: Date.now() / 1000 + 90,
      },
    },
    iceServers: [],
  })
}

it('the distributed client treats a renewal refusal as terminal and preserves the update message', async () => {
  const fetch = vi.fn(async url => url.endsWith('/authorize')
    ? grant()
    : url.endsWith('/renew')
      ? Response.json({ error: 'Update required: update the app.' }, { status: 426 })
      : new Response(null, { status: 204 }))
  const { context } = await connected(fetch)
  await vi.advanceTimersByTimeAsync(60001)
  expect(context.protocolError).toContain('update the app')
  const calls = fetch.mock.calls.length
  await vi.advanceTimersByTimeAsync(300000)
  expect(fetch).toHaveBeenCalledTimes(calls)
})

it('the distributed client does not replay an uncertain message after an UpdateRequired control frame', async () => {
  const fetch = vi.fn(async url => url.endsWith('/authorize') ? grant() : new Response(null, { status: 204 }))
  const { transport, context, channel } = await connected(fetch)
  const pending = transport.request('/chats/c/messages', '/api/chats/c/messages', { method: 'POST', body: '{"id":"uncertain","text":"Once"}' })
  const rejected = expect(pending).rejects.toThrow('Update required')
  await vi.waitFor(() => expect(channel.packets).toHaveLength(1))
  receive(channel, { type: 'update_required', message: 'Update required: update the app.' })
  await rejected
  expect(context.protocolError).toContain('Update required')
  await vi.advanceTimersByTimeAsync(300000)
  expect(fetch.mock.calls.every(([url]) => url.includes('/direct/'))).toBe(true)
})
