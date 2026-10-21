import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { agentPluginDirectory, agentPluginManifest } from '../src/agent-plugin-directory'
import { ApiError } from '../src/api'

const { request } = vi.hoisted(() => ({ request: vi.fn() }))
vi.mock('../src/api', () => ({
  api: request,
  ApiError: class extends Error {
    constructor(message: string, readonly status: number) { super(message) }
  },
}))

const signal = () => new AbortController().signal

function card(id: string) {
  return {
    id,
    status: 'active',
    active: '1',
    presentation: {
      name: id,
      agentAccess: true,
      agentAccessDescription: 'Package-owned access help',
      contributions: { operations: 2 },
    },
  }
}

function instance(id: string) {
  return {
    id,
    pluginId: 'custom',
    name: id,
    enabled: false,
    agentId: 'delegate',
    agentIds: ['root'],
    resourceAgentIds: ['granular'],
    status: { state: 'needs_review' },
  }
}

const page = (items: unknown[], nextCursor: string | null = null) => ({ items, nextCursor })

beforeEach(() => {
  request.mockReset()
})

it('completes compact package, instance and grant-count pages, preserving unavailable held access', async () => {
  request.mockImplementation(async (path: string) => {
    switch (path) {
      case '/plugins/packages': return { ...page([card('custom')], 'custom'), packageReads: true, agentReads: true }
      case '/plugins/packages?cursor=custom': return page([{
        id: 'unavailable',
        status: 'invalid',
        active: null,
        presentation: null,
      }])
      case '/plugins/instance-access': return page([instance('one')], 'one')
      case '/plugins/instance-access?cursor=one': return page([{ ...instance('two'), pluginId: 'removed-package' }])
      case '/plugins/agent-grant-counts': return page([{ id: 'one', count: 3 }], 'one')
      case '/plugins/agent-grant-counts?cursor=one': return page([{ id: 'two', count: 1 }])
      default: throw new Error(`Unexpected read ${path}`)
    }
  })
  const result = await agentPluginDirectory(signal())
  expect(result.packages.map(item => item.id)).toEqual(['custom', 'unavailable'])
  expect(result.connections.map(item => item.id)).toEqual(['one', 'two'])
  expect(result.connections[0].agentId).toBe('delegate')
  expect(result.connections[0].status.state).toBe('needs_review')
  expect(result.resourceGrantCounts).toEqual({ one: 3, two: 1 })
  expect(result.packages[0].agentAccessDescription).toBe('Package-owned access help')
  expect(request.mock.calls.every(([path]) => path !== '/plugins' && !path.includes('/plugins/packages/custom'))).toBe(true)
  request.mockResolvedValueOnce({ id: 'custom', active: '1', manifest: { id: 'custom', operations: [] } })
  const manifest = await agentPluginManifest(result, 'custom', signal())
  expect(manifest.id).toBe('custom')
  expect(request.mock.lastCall?.[0]).toBe('/plugins/packages/custom')
})

it('falls back only for explicit older-host support and retains unknown resource totals', async () => {
  for (const support of [new ApiError('Missing', 404), { packageReads: true, items: [], agentReads: false }]) {
    request.mockReset()
    if (support instanceof Error)
      request.mockRejectedValueOnce(support)
    else
      request.mockResolvedValueOnce(support)
    request.mockResolvedValueOnce({
      packages: [{
        id: 'old',
        status: 'active',
        active: '1',
        manifest: {
          id: 'old',
          name: 'Old',
          agentAccess: { description: 'Keep old help' },
          operations: [{}],
        },
      }],
      connections: [instance('held')],
    })
    const result = await agentPluginDirectory(signal())
    expect(result.resourceGrantCounts).toBeUndefined()
    expect(result.packages[0].operations).toBe(1)
    const before = request.mock.calls.length
    expect((await agentPluginManifest(result, 'old', signal())).id).toBe('old')
    expect(request).toHaveBeenCalledTimes(before)
  }
})

it('never falls back or returns partial saved access when a supported read fails', async () => {
  request.mockResolvedValueOnce({ packageReads: true, agentReads: true, ...page([card('custom')]) })
    .mockResolvedValueOnce(page([instance('one')], 'one'))
    .mockRejectedValueOnce(new ApiError('Later page offline', 503))
  await expect(agentPluginDirectory(signal())).rejects.toThrow('Later page offline')
  expect(request.mock.calls.every(([path]) => path !== '/plugins')).toBe(true)
  request.mockReset()
  request.mockResolvedValueOnce({ packageReads: true, agentReads: true, ...page([card('custom')]) })
    .mockResolvedValueOnce(page([instance('one')]))
    .mockResolvedValueOnce(page([{ id: 'one', count: -1 }]))
  await expect(agentPluginDirectory(signal())).rejects.toThrow('completely')
})
