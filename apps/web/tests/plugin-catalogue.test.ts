import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { ApiError } from '../src/api'
import {
  pluginCards,
  pluginConfiguration,
  pluginInstance,
  pluginReview,
} from '../src/plugin-catalogue'

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

it('loads only the selected configuration and completes both instance and cross-package access pages', async () => {
  request.mockResolvedValueOnce({
    id: 'chosen',
    builtin: false,
    status: 'active',
    active: '1',
    manifest: { name: 'Chosen' },
    instanceReads: true,
    accessReads: true,
    capabilities: [],
    setupDrafts: true,
  })
    .mockResolvedValueOnce({ items: [{ id: 'one', pluginId: 'chosen' }], nextCursor: 'one' })
    .mockResolvedValueOnce({ items: [{ id: 'two', pluginId: 'chosen' }], nextCursor: null })
    .mockResolvedValueOnce({ items: [], nextCursor: 'boundary' })
    .mockResolvedValueOnce({
      items: [{
        id: 'elsewhere',
        pluginId: 'other',
        agentIds: ['root'],
        resourceAgentIds: ['resource'],
      }],
      nextCursor: null,
    })
  const result = await pluginConfiguration('chosen', new AbortController().signal)
  expect(result.connections.map(instance => instance.id)).toEqual(['one', 'two'])
  expect(result.access[0].resourceAgentIds).toEqual(['resource'])
  expect(result.setupDrafts).toBe(true)
  expect(result.exactInstanceReads).toBe(true)
  expect(request.mock.calls.map(call => call[0])).toEqual([
    '/plugins/packages/chosen',
    '/plugins/packages/chosen/connections',
    '/plugins/packages/chosen/connections?cursor=one',
    '/plugins/instance-access',
    '/plugins/instance-access?cursor=boundary',
  ])
})

it('keeps a supported configuration failure unknown and never falls back after a later read fails', async () => {
  const detail = {
    id: 'chosen',
    manifest: null,
    instanceReads: true,
    accessReads: true,
    capabilities: [],
  }
  for (const response of [new ApiError('offline', 503), new ApiError('gone', 404)]) {
    request.mockResolvedValueOnce(detail).mockRejectedValueOnce(response)
    await expect(pluginConfiguration('chosen', new AbortController().signal)).rejects.toBe(response)
  }

  request.mockResolvedValueOnce(detail).mockResolvedValueOnce({ items: [] }).mockResolvedValueOnce({ items: [{ id: 'bad', pluginId: 'other', agentIds: [] }] })
  await expect(pluginConfiguration('chosen', new AbortController().signal)).rejects.toThrow('incomplete')
  expect(request.mock.calls.some(call => call[0] === '/plugins')).toBe(false)
})

it('uses the explicit older-host fallback while keeping all saved root and global resource access', async () => {
  request.mockResolvedValueOnce({ id: 'chosen', manifest: null, instanceReads: true })
    .mockResolvedValueOnce({
      packages: [{ id: 'chosen', manifest: null, versions: [] }, { id: 'other', manifest: { name: 'Other' }, versions: [] }],
      connections: [{ id: 'mine', pluginId: 'chosen', agentIds: [] }, { id: 'elsewhere', pluginId: 'other', agentIds: ['agent'] }],
      capabilities: [],
      resourceGrantCounts: { personal: 2 },
    })
  const result = await pluginConfiguration('chosen', new AbortController().signal)
  expect(result.exactInstanceReads).toBe(false)
  expect(result.connections.map(item => item.id)).toEqual(['mine'])
  expect(result.access[1].agentIds).toEqual(['agent'])
  expect(result.resourceAgentIds).toEqual(['personal'])
  expect(result.entry).not.toHaveProperty('versions')
})

it('re-reads complete current root grants and refuses a deleted or mismatched instance', async () => {
  request.mockResolvedValueOnce({ id: 'one', pluginId: 'chosen', agentIds: ['concurrent', 'saved'] })
  expect((await pluginInstance('chosen', 'one', true)).agentIds).toEqual(['concurrent', 'saved'])
  expect(request.mock.calls[0][0]).toBe('/plugins/packages/chosen/connections/one')
  request.mockRejectedValueOnce(new ApiError('Deleted', 404))
  await expect(pluginInstance('chosen', 'one', true)).rejects.toThrow('Deleted')
  request.mockResolvedValueOnce({ connections: [] })
  await expect(pluginInstance('chosen', 'one', false)).rejects.toThrow('removed')
  request.mockResolvedValueOnce({ id: 'one', pluginId: 'wrong', agentIds: [] })
  await expect(pluginInstance('chosen', 'one', true)).rejects.toThrow('incomplete')
})

it('clears a removed package using bounded support discovery and never loads the global catalogue', async () => {
  request.mockRejectedValueOnce(new ApiError('not found', 404))
    .mockResolvedValueOnce({
      packageReads: true,
      instanceReads: true,
      accessReads: true,
      items: [],
      capabilities: [],
    })
  const result = await pluginConfiguration('removed', new AbortController().signal)
  expect(result.entry).toBeNull()
  expect(result.exactInstanceReads).toBe(true)
  expect(request.mock.calls.map(call => call[0])).toEqual(['/plugins/packages/removed', '/plugins/packages'])
  request.mockRejectedValueOnce(new ApiError('not found', 404)).mockRejectedValueOnce(new ApiError('offline', 503))
  await expect(pluginConfiguration('removed', new AbortController().signal)).rejects.toThrow('offline')
})
