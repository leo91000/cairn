import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { api, ApiError } from '../src/api'
import { useResourceSave } from '../src/resourceSave'

vi.mock('../src/api', () => ({
  api: vi.fn(),
  ApiError: class extends Error {
    constructor(message: string, readonly status: number) { super(message) }
  },
}))

beforeEach(() => vi.mocked(api).mockReset())

it('replays an uncertain creation unchanged and applies later edits to the acquired id', async () => {
  const saver = useResourceSave()
  const form = { name: 'Original mission', cron: '0 9 * * 1' }
  vi.mocked(api)
    .mockRejectedValueOnce(new ApiError('', 0))
    .mockResolvedValueOnce({ id: 'created-id', ...form })
    .mockRejectedValueOnce(new ApiError('Invalid schedule', 400))
    .mockResolvedValueOnce({ id: 'created-id', name: 'Edited mission', cron: null })

  await expect(saver.save('/tasks', { body: () => form })).rejects.toThrow()
  const originalBody = vi.mocked(api).mock.calls[0][1]!.body
  form.name = 'Edited mission'
  form.cron = 'invalid'
  await expect(saver.save('/tasks', { body: () => form })).rejects.toThrow('Invalid schedule')
  expect(vi.mocked(api).mock.calls[1]).toEqual(['/tasks', { method: 'POST', body: originalBody }])
  expect(vi.mocked(api).mock.calls[2]).toEqual(['/tasks/created-id', { method: 'PUT', body: JSON.stringify(form) }])
  expect(saver.savedId.value).toBe('created-id')

  form.cron = ''
  await saver.save('/tasks', { body: () => form })
  expect(vi.mocked(api).mock.calls[3][0]).toBe('/tasks/created-id')
  expect(vi.mocked(api).mock.calls.filter(([, options]) => options?.method === 'POST')).toHaveLength(2)
})

it('allows correction after a rejected creation without changing the session identity', async () => {
  const saver = useResourceSave()
  const form = { path: '/missing' }
  vi.mocked(api)
    .mockRejectedValueOnce(new ApiError('Directory does not exist', 400))
    .mockResolvedValue({ id: 'project-id' })

  await expect(saver.save('/projects', { body: () => form })).rejects.toThrow('Directory does not exist')
  expect(saver.pendingCreation.value).toBe(false)
  form.path = '/existing'
  await saver.save('/projects', { body: () => form })

  const bodies = vi.mocked(api).mock.calls.filter(([, options]) => options?.method === 'POST').map(([, options]) => JSON.parse(options!.body as string))
  expect(bodies[1]).toEqual({ path: '/existing', creationId: bodies[0].creationId })
})

it('retains a server-error creation payload and resets the identity for the next editor session', async () => {
  const saver = useResourceSave()
  vi.mocked(api)
    .mockRejectedValueOnce(new ApiError('Response failed after commit', 500))
    .mockResolvedValueOnce({ id: 'second-agent' })
  await expect(saver.save('/agents', { body: () => ({ name: 'First' }) })).rejects.toThrow()
  expect(saver.pendingCreation.value).toBe(true)
  const firstIdentity = JSON.parse(vi.mocked(api).mock.calls[0][1]!.body as string).creationId

  saver.reset()
  await saver.save('/agents', { body: () => ({ name: 'Second' }) })
  const secondBody = JSON.parse(vi.mocked(api).mock.calls[1][1]!.body as string)
  expect(secondBody.name).toBe('Second')
  expect(secondBody.creationId).not.toBe(firstIdentity)
})

it('applies current edits after an uncertain creation followed by a rejected replay', async () => {
  const saver = useResourceSave()
  const form = { name: 'Original project' }
  vi.mocked(api)
    .mockRejectedValueOnce(new ApiError('', 0))
    .mockRejectedValueOnce(new ApiError('Rate limited', 429))
    .mockResolvedValueOnce({ id: 'existing-project', name: 'Original project' })
    .mockResolvedValueOnce({ id: 'existing-project', name: 'Latest project' })

  await expect(saver.save('/projects', { body: () => form })).rejects.toThrow()
  await expect(saver.save('/projects', { body: () => form })).rejects.toThrow('Rate limited')
  expect(saver.pendingCreation.value).toBe(true)
  form.name = 'Latest project'
  const saved = await saver.save<{ id: string, name: string }>('/projects', { body: () => form })
  expect(saved.name).toBe('Latest project')
  expect(vi.mocked(api).mock.calls[3]).toEqual(['/projects/existing-project', { method: 'PUT', body: JSON.stringify(form) }])
})

it('treats a timeout response as an uncertain creation', async () => {
  const saver = useResourceSave()
  const form = { name: 'Original mission' }
  vi.mocked(api)
    .mockRejectedValueOnce(new ApiError('Timed out', 408))
    .mockResolvedValueOnce({ id: 'created-mission' })
    .mockResolvedValueOnce({ id: 'created-mission' })

  await expect(saver.save('/tasks', { body: () => form })).rejects.toThrow('Timed out')
  expect(saver.pendingCreation.value).toBe(true)
  const originalBody = vi.mocked(api).mock.calls[0][1]!.body
  form.name = 'Updated mission'
  await saver.save('/tasks', { body: () => form })
  expect(vi.mocked(api).mock.calls[1][1]!.body).toBe(originalBody)
})

it('recovers a GitHub import before updating its editable project fields', async () => {
  const saver = useResourceSave()
  const form = { name: 'Imported project', path: '', sourceMode: 'remote' }
  const options = {
    body: () => form,
    createPath: '/projects/github',
    createBody: () => ({ repository: 'owner/repository', name: 'Imported project' }),
    onCreated: (saved: { id: string, path: string, sourceMode: string }) => {
      expect(saver.savedId.value).toBe(saved.id)
      form.path = saved.path
      form.sourceMode = saved.sourceMode
    },
  }
  vi.mocked(api)
    .mockRejectedValueOnce(new ApiError('', 0))
    .mockResolvedValueOnce({ id: 'imported-project', path: '/cloned/repository', sourceMode: 'local' })
    .mockResolvedValueOnce({ id: 'imported-project', ...form, name: 'Renamed project' })

  await expect(saver.save('/projects', options)).rejects.toThrow()
  form.name = 'Renamed project'
  await saver.save('/projects', options)
  expect(vi.mocked(api).mock.calls[1]).toEqual(vi.mocked(api).mock.calls[0])
  expect(vi.mocked(api).mock.calls[2]).toEqual(['/projects/imported-project', {
    method: 'PUT',
    body: JSON.stringify({ name: 'Renamed project', path: '/cloned/repository', sourceMode: 'local' }),
  }])
})

it('recovers a committed creation whose response lacks its identity', async () => {
  const saver = useResourceSave()
  const form = { name: 'Created agent' }
  vi.mocked(api)
    .mockResolvedValueOnce({})
    .mockResolvedValue({ id: 'created-agent' })

  await expect(saver.save('/agents', { body: () => form })).rejects.toThrow('resource identity')
  expect(saver.savedId.value).toBeNull()
  expect(saver.pendingCreation.value).toBe(true)
  await saver.save('/agents', { body: () => form })
  expect(vi.mocked(api).mock.calls[1]).toEqual(vi.mocked(api).mock.calls[0])
  expect(saver.savedId.value).toBe('created-agent')
})
