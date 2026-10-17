import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'

vi.mock('../src/installation-transport', () => ({
  InstallationTransport: class {
    start() {}
    stop() {}
    availabilityChanged() {}
    request(_path: string, url: string, options: RequestInit) { return fetch(url, options) }
  },
}))
vi.mock('../src/history-cache', () => ({ clearHistoryCache: async () => {} }))

const request = vi.fn()
let client: typeof import('../src/api')

beforeEach(async () => {
  vi.resetModules()
  vi.useFakeTimers()
  request.mockReset()
  vi.stubGlobal('fetch', request)
  client = await import('../src/api')
  Object.assign(client.state, {
    installationId: 'first',
    accountId: 'owner',
    csrf: 'fixture',
    authenticated: true,
    ready: true,
  })
})
afterEach(() => {
  client.state.authenticated = false
  vi.clearAllTimers()
  vi.useRealTimers()
  vi.unstubAllGlobals()
})

function json(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), { status, headers: { 'Content-Type': 'application/json' } })
}

function successful(url: string) {
  return json(url.endsWith('/overview') || url.endsWith('/settings') ? {} : [])
}

it('keeps failed reads quiet and loads their data automatically after a shared recovery probe', async () => {
  let down = true
  request.mockImplementation((url: string) => down ? json({ error: 'Installation busy.' }, 503) : successful(url))
  const settings = client.api('/settings')
  const audit = client.api('/audit')
  await vi.advanceTimersByTimeAsync(0)
  expect(client.state.connectionLost).toBe(true)
  expect(client.state.error).toBe('')
  down = false
  await vi.advanceTimersByTimeAsync(1000)
  await expect(settings).resolves.toEqual({})
  await expect(audit).resolves.toEqual([])
  expect(client.state.connectionLost).toBe(false)
  expect(request.mock.calls.filter(([url]) => url.endsWith('/overview'))).toHaveLength(1)
  expect(request.mock.calls.filter(([url]) => url.endsWith('/settings'))).toHaveLength(2)
})

it('does not replay a failed mutation and reports it through a short toast', async () => {
  request.mockImplementation((url: string, options: RequestInit) => options.method === 'POST'
    ? json({ error: 'Installation unavailable' }, 503)
    : successful(url))
  await expect(client.api('/tasks', { method: 'POST', body: '{}' })).rejects.toMatchObject({ message: '' })
  expect(client.state.toast).toBe('Connection unavailable. Please try again.')
  expect(client.state.connectionLost).toBe(true)
  await vi.advanceTimersByTimeAsync(1000)
  expect(client.state.connectionLost).toBe(false)
  expect(request.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(1)
})

it.each(['network', 'proxy', 'json-null'])('recovers a %s failure without a Cairn error body', async (kind) => {
  request.mockImplementationOnce(() => kind === 'network'
    ? Promise.reject(new TypeError('Failed to fetch'))
    : kind === 'proxy'
      ? new Response('<h1>Bad gateway</h1>', { status: 502 })
      : json(null, 503))
  request.mockImplementation((url: string) => successful(url))
  const result = client.api('/settings')
  await vi.advanceTimersByTimeAsync(0)
  expect(client.state.connectionLost).toBe(true)
  await vi.advanceTimersByTimeAsync(1000)
  await expect(result).resolves.toEqual({})
  expect(client.state.connectionLost).toBe(false)
})

it('preserves genuine application errors without starting recovery', async () => {
  request.mockResolvedValue(json({ error: 'GitHub unavailable' }, 503))
  await expect(client.api('/projects')).rejects.toMatchObject({ message: 'GitHub unavailable', status: 503 })
  expect(client.state.connectionLost).toBe(false)
  expect(vi.getTimerCount()).toBe(0)
})

it('cancels a pending read immediately while the shared connection probe can continue', async () => {
  request.mockResolvedValueOnce(json({ error: 'Installation unavailable' }, 503))
  request.mockImplementation((url: string) => successful(url))
  const controller = new AbortController()
  const result = client.api('/settings', { signal: controller.signal })
  const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' })
  await vi.advanceTimersByTimeAsync(0)
  controller.abort()
  await rejected
  await vi.advanceTimersByTimeAsync(1000)
  expect(request.mock.calls.filter(([url]) => url.endsWith('/settings'))).toHaveLength(1)
})

it('cancels old recovery and starts a separate probe after switching installations', async () => {
  let down = true
  request.mockImplementation((url: string) => down ? json({ error: 'Installation unavailable' }, 503) : successful(url))
  const first = client.api('/settings')
  const firstRejected = expect(first).rejects.toMatchObject({ message: '' })
  await vi.advanceTimersByTimeAsync(0)
  client.state.installationId = 'second'
  await firstRejected
  const second = client.api('/settings')
  await vi.advanceTimersByTimeAsync(0)
  down = false
  await vi.advanceTimersByTimeAsync(1000)
  await expect(second).resolves.toEqual({})
  expect(client.state.connectionLost).toBe(false)
  expect(request.mock.calls.filter(([url]) => url.includes('/first/') && url.endsWith('/overview'))).toHaveLength(0)
  expect(request.mock.calls.filter(([url]) => url.includes('/second/') && url.endsWith('/overview'))).toHaveLength(1)
})

it('preserves account-only failures without probing an absent installation', async () => {
  client.state.installationId = ''
  const failure = new TypeError('Failed to fetch')
  request.mockRejectedValueOnce(failure)
  await expect(client.accountApi('/options')).rejects.toBe(failure)
  expect(client.state.connectionLost).toBe(false)
  expect(request).toHaveBeenCalledTimes(1)
  expect(request.mock.calls[0][0]).toBe('/api/account/options')
  expect(vi.getTimerCount()).toBe(0)
})

it('keeps retries bounded when one list stays unavailable while probes succeed', async () => {
  let down = true
  request.mockImplementation((url: string) => down && url.endsWith('/agents')
    ? json({ error: 'Installation busy.' }, 503)
    : successful(url))
  const result = client.api('/agents')
  await vi.advanceTimersByTimeAsync(0)
  for (let probe = 1; probe <= 3; probe++) {
    await vi.advanceTimersByTimeAsync(1000)
    expect(request.mock.calls.filter(([url]) => url.endsWith('/agents'))).toHaveLength(probe + 1)
  }

  expect(request.mock.calls.filter(([url]) => url.endsWith('/projects'))).toHaveLength(0)
  down = false
  await vi.advanceTimersByTimeAsync(1000)
  await expect(result).resolves.toEqual([])
})

it('reconciles an uncertain mutation once even if a list needs several recovery probes', async () => {
  let down = true
  request.mockImplementation((url: string, options: RequestInit) => options.method === 'POST' || (down && url.endsWith('/agents'))
    ? json({ error: 'Installation busy.' }, 503)
    : successful(url))
  await expect(client.api('/tasks', { method: 'POST', body: '{}' })).rejects.toMatchObject({ message: '' })
  for (let probe = 1; probe <= 3; probe++) {
    await vi.advanceTimersByTimeAsync(1000)
    expect(request.mock.calls.filter(([url]) => url.endsWith('/agents'))).toHaveLength(probe)
  }

  expect(request.mock.calls.filter(([url]) => url.endsWith('/projects'))).toHaveLength(1)
  expect(request.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(1)
  down = false
  await vi.advanceTimersByTimeAsync(1000)
  expect(client.state.connectionLost).toBe(false)
})

it('keeps the failed-action toast when the real read marker reports its quiet error', async () => {
  request.mockImplementation((url: string, options: RequestInit) => options.method === 'POST'
    ? json({ error: 'Installation unavailable' }, 503)
    : successful(url))
  const { markRead, localReads } = await import('../src/reads')
  await markRead([{ key: 'chat:fixture', at: 123 }])
  expect(localReads['chat:fixture']).toBeUndefined()
  expect(client.state.toast).toBe('Connection unavailable. Please try again.')
  await vi.advanceTimersByTimeAsync(4500)
  expect(client.state.toast).toBe('')
  expect(request.mock.calls.filter(([, options]) => options.method === 'POST')).toHaveLength(1)
})

it.each(['Installation request timed out.', 'Installation response timed out.'])('recovers the dispatcher deadline response %s', async (message) => {
  request.mockResolvedValueOnce(json({ error: message }, 504))
  request.mockImplementation((url: string) => successful(url))
  const result = client.api('/settings')
  await vi.advanceTimersByTimeAsync(0)
  expect(client.state.connectionLost).toBe(true)
  await vi.advanceTimersByTimeAsync(1000)
  await expect(result).resolves.toEqual({})
  expect(client.state.connectionLost).toBe(false)
})

it('recovers pending reads even after the availability poll marked the installation offline', async () => {
  client.state.installationOnline = false
  request.mockRejectedValueOnce(new TypeError('Failed to fetch'))
  request.mockImplementation((url: string) => successful(url))
  const result = client.api('/settings')
  await vi.advanceTimersByTimeAsync(0)
  expect(client.state.connectionLost).toBe(true)
  expect(client.state.error).toBe('')
  client.state.installationOnline = true
  await vi.advanceTimersByTimeAsync(1000)
  await expect(result).resolves.toEqual({})
  expect(client.state.connectionLost).toBe(false)
  expect(request.mock.calls.filter(([url]) => url.endsWith('/settings'))).toHaveLength(2)
})
