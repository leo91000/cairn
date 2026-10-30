import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { api } from '../src/api'
import { pluginRequest } from '../src/plugin-operations'

vi.mock('../src/api', async (original) => {
  const actual = await original<typeof import('../src/api')>()
  return { ...actual, api: vi.fn() }
})

beforeEach(() => vi.mocked(api).mockReset())

it.each([
  ['project', 'project'],
  ['sourceProjects', 'page'],
  ['skillImport', 'imported'],
  ['skillSource', 'status'],
  ['skillBundle', 'bundle'],
])('delivers the completed %s preparation to the existing resource editor', async (kind, field) => {
  const value = { revision: 'latest' }
  vi.mocked(api).mockResolvedValueOnce({ operation: 'job' }).mockResolvedValueOnce({
    id: 'job',
    state: 'completed',
    result: { kind, [field]: value },
    error: null,
  })
  expect(await pluginRequest('/resource', { method: 'POST' })).toEqual(value)
  expect(vi.mocked(api).mock.calls[1]![0]).toBe('/plugins/packages/operations/job')
})

it('preserves a preparation revision conflict for the resource editor', async () => {
  vi.mocked(api).mockResolvedValueOnce({ operation: 'job' }).mockResolvedValueOnce({
    id: 'job',
    state: 'failed',
    result: null,
    error: 'Reload the skill',
    errorStatus: 409,
  })
  await expect(pluginRequest('/resource', { method: 'POST' })).rejects.toMatchObject({ status: 409, message: 'Reload the skill' })
})
