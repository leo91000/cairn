import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { nextTick } from 'vue'

class Peer {
  createDataChannel() {
    return Object.assign(new EventTarget(), { close() {} })
  }

  async createOffer() {
    return { sdp: `v=0\r\na=fingerprint:sha-256 ${'AA:'.repeat(31)}AA\r\n` }
  }

  close() {}
}

beforeEach(() => {
  vi.resetModules()
  vi.stubGlobal('window', Object.assign(new EventTarget(), {
    location: { pathname: '/installations/install/mcps/callback', origin: 'https://cairn.test', assign: vi.fn() },
  }))
  vi.stubGlobal('document', Object.assign(new EventTarget(), {
    getElementById: () => ({ hasAttribute: () => true }),
    hidden: false,
  }))
  vi.stubGlobal('navigator', { onLine: true })
  vi.stubGlobal('RTCPeerConnection', Peer)
})

afterEach(async () => {
  const { state } = await import('../src/api')
  state.authenticated = false
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
})

it.each(['redirect', 'pagehide'] as const)('keeps direct signaling retired after %s until a persisted pageshow', async (departure) => {
  const fetch = vi.fn().mockImplementation(() => Promise.resolve(Response.json({ available: false })))
  vi.stubGlobal('fetch', fetch)
  const { redirect, state } = await import('../src/api')
  const { documentLifetime } = await import('../src/document-lifetime')
  Object.assign(state, { installationId: 'install', authenticated: true, ready: true })
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledOnce())
  const lifetime = documentLifetime.value

  if (departure === 'redirect')
    redirect('https://cairn.test/installations/quiet-cedar/mcps/callback')
  else
    window.dispatchEvent(new Event('pagehide'))

  expect(lifetime.signal.aborted).toBe(true)
  window.dispatchEvent(Object.assign(new Event('pageshow'), { persisted: false }))
  await nextTick()
  await new Promise(resolve => setTimeout(resolve, 0))

  expect(fetch).toHaveBeenCalledOnce()
  expect(documentLifetime.value).toBe(lifetime)
  expect(state.redirecting).toBe(departure === 'redirect')

  // The initial event must not consume the later Back restoration listener.
  window.dispatchEvent(Object.assign(new Event('pageshow'), { persisted: true }))
  await vi.waitFor(() => expect(fetch).toHaveBeenCalledTimes(2))
  expect(documentLifetime.value).not.toBe(lifetime)
  expect(documentLifetime.value.signal.aborted).toBe(false)
  expect(state.redirecting).toBe(false)

  const restoredLifetime = documentLifetime.value
  window.dispatchEvent(Object.assign(new Event('pageshow'), { persisted: true }))
  await nextTick()
  expect(documentLifetime.value).toBe(restoredLifetime)
  expect(fetch).toHaveBeenCalledTimes(2)
})
