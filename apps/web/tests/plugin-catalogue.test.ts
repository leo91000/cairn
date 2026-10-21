import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { ApiError } from '../src/api'
import { pluginCards, pluginReview } from '../src/plugin-catalogue'

const { request } = vi.hoisted(() => ({ request: vi.fn() }))
vi.mock('../src/api', () => ({
  api: request,
  ApiError: class extends Error {
    constructor(message: string, readonly status: number) { super(message) }
  },
}))

function card(id: string) {
  return {
    id,
    builtin: false,
    active: null,
    status: 'needs_approval',
    presentation: null,
    versionCount: 0,
    summary: {
      instances: 0,
      states: {},
      pendingRequests: 0,
      authorizedAgents: 0,
    },
  }
}

function page(items: unknown[], nextCursor: string | null = null) {
  return {
    packageReads: true,
    instanceReads: true,
    capabilities: [],
    items,
    nextCursor,
  }
}

beforeEach(() => request.mockReset())

it('completes all card pages, including an empty intermediate page, without reading full settings', async () => {
  request.mockResolvedValueOnce(page([card('one')], 'id/value'))
    .mockResolvedValueOnce(page([], 'last'))
    .mockResolvedValueOnce(page([card('two')]))
  const result = await pluginCards(new AbortController().signal)
  expect(result.items.map(item => item.id)).toEqual(['one', 'two'])
  expect(result.nextCursor).toBeNull()
  expect(request.mock.calls.map(call => call[0])).toEqual([
    '/plugins/packages',
    '/plugins/packages?cursor=id%2Fvalue',
    '/plugins/packages?cursor=last',
  ])
})

it('keeps failed, malformed or repeated pages unknown instead of falling back to a partial list', async () => {
  for (const failure of [new ApiError('denied', 403), new ApiError('busy', 503)]) {
    request.mockRejectedValueOnce(failure)
    await expect(pluginCards(new AbortController().signal)).rejects.toBe(failure)
  }

  request.mockResolvedValueOnce(page([card('one')], 'next')).mockRejectedValueOnce(new ApiError('offline', 503))
  await expect(pluginCards(new AbortController().signal)).rejects.toThrow('offline')
  request.mockResolvedValueOnce(page([card('one')], 'next')).mockResolvedValueOnce(page([], 'next'))
  await expect(pluginCards(new AbortController().signal)).rejects.toThrow('changed while loading')
  request.mockResolvedValueOnce(page([{ ...card('one'), summary: undefined }]))
  await expect(pluginCards(new AbortController().signal)).rejects.toThrow('summaries')
  expect(request.mock.calls.every(call => call[0] !== '/plugins')).toBe(true)
})

it('falls back for an older host and never presents global resource totals as a package count', async () => {
  request.mockRejectedValueOnce(new ApiError('not found', 404)).mockResolvedValueOnce({
    packages: [{
      id: 'old',
      builtin: false,
      status: 'active',
      active: '1',
      manifest: null,
      versions: [],
    }],
    capabilities: [],
    connections: [{
      id: 'instance',
      pluginId: 'old',
      enabled: false,
      status: { state: 'connected' },
      requests: [],
      agentIds: ['one'],
    }],
    resourceGrantCounts: { two: 3 },
  })
  const result = await pluginCards(new AbortController().signal)
  expect(result.packageReads).toBe(false)
  expect(result.items[0].summary).toEqual({
    instances: 1,
    states: { paused: 1 },
    pendingRequests: 0,
    authorizedAgents: null,
  })
})

it('stops following pages when the caller closes the screen', async () => {
  const controller = new AbortController()
  request.mockImplementationOnce(() => {
    controller.abort()
    return page([], 'next')
  })
  await expect(pluginCards(controller.signal)).rejects.toThrow()
  expect(request).toHaveBeenCalledTimes(1)
})

it('loads configuration and complete version metadata separately from exact reviewed files', async () => {
  request.mockResolvedValueOnce({
    id: 'plugin',
    builtin: false,
    status: 'update',
    active: '1',
    manifest: { name: 'Current' },
  })
    .mockResolvedValueOnce({ items: [{ version: '1', digest: 'old', approved: true }], nextCursor: '1' })
    .mockResolvedValueOnce({ items: [{ version: '2', digest: 'new', approved: false }, { version: 'bad', error: 'Invalid module' }] })
  const result = await pluginReview('plugin', new AbortController().signal)
  expect(result.lazy).toBe(true)
  expect(result.entry.versions.map(version => version.version)).toEqual(['1', '2', 'bad'])
  expect(result.entry.versions[1].manifest).toBeUndefined()
  expect(request.mock.calls.map(call => call[0])).toEqual([
    '/plugins/packages/plugin',
    '/plugins/packages/plugin/versions',
    '/plugins/packages/plugin/versions?cursor=1',
  ])
})
