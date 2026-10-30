import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import { ref } from 'vue'

const request = vi.hoisted(() => vi.fn())
vi.mock('../src/api', () => ({ api: request, state: { installationId: 'fixture-installation', csrf: 'csrf' } }))

afterEach(() => {
  request.mockReset()
  vi.unstubAllGlobals()
})

it('keeps a pending change and its refusal on the agent it was made for', async () => {
  vi.stubGlobal('fetch', async () => new Response(JSON.stringify({ members: [{ id: 'ada', email: 'ada@example.test' }] })))
  let refuse!: (cause: Error) => void
  request.mockImplementation((path: string, options?: RequestInit) => options?.method === 'PUT'
    ? new Promise((_, reject) => refuse = reject)
    : Promise.resolve({ accounts: [] }))
  const { useAgentPeople } = await import('../src/agent-people')
  const agent = ref<string | null>('reviewer')
  const people = useAgentPeople(agent)
  await people.load()

  const change = people.setAccess('ada', true)
  expect(people.saving.value).toEqual(['ada'])
  agent.value = 'writer'
  await people.load()
  expect(people.saving.value).toEqual([])

  refuse(new Error('Access could not be changed.'))
  await change
  expect(people.failure.value).toBeNull()
  agent.value = 'reviewer'
  expect(people.saving.value).toEqual([])
})
