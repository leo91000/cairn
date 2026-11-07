import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'

const mocked = vi.hoisted(() => ({ api: vi.fn(), notify: vi.fn() }))
vi.mock('../src/api', async () => {
  const { reactive } = await import('vue')
  return {
    ...mocked,
    state: reactive({
      accountId: 'a',
      installationId: 'i',
      authenticated: true,
      signingOut: false,
    }),
  }
})
const { state } = await import('../src/api')
const { localReads, markRead } = await import('../src/reads')

beforeEach(() => {
  vi.resetAllMocks()
  state.accountId = `account-${Math.random()}`
  state.installationId = 'i'
  state.authenticated = true
  state.signingOut = false
  mocked.api.mockResolvedValue({})
})

it('sends the observed completion and batches bulk reads within the API limit', async () => {
  const items = Array.from({ length: 401 }, (_, index) => ({ key: `chat:${index}`, at: index + 1 }))
  await markRead(items)
  const batches = mocked.api.mock.calls.map(([, options]) => JSON.parse(options.body).items)
  expect(batches.map(batch => batch.length)).toEqual([200, 200, 1])
  expect(batches[0][0]).toEqual({ target: { kind: 'chat', chatId: '0' }, finishedAt: 1 })
  expect(mocked.api.mock.calls[0][0]).toBe('/reads')
  expect(Object.keys(localReads)).toHaveLength(401)
})

it('keeps successful batches when a later batch fails', async () => {
  mocked.api.mockResolvedValueOnce({}).mockRejectedValueOnce(new Error('Offline'))
  await markRead(Array.from({ length: 201 }, (_, index) => ({ key: `chat:${index}`, at: 10 })))
  expect(Object.keys(localReads)).toHaveLength(200)
  expect(localReads['chat:200']).toBeUndefined()
  expect(mocked.notify).toHaveBeenCalledWith('Offline')
})

it('clears optimistic marks across accounts and installations and ignores stale failures', async () => {
  let fail!: (error: Error) => void
  mocked.api.mockImplementationOnce(() => new Promise((_, reject) => fail = reject))
  const old = markRead([{ key: 'chat:shared', at: 10 }])
  state.accountId = 'b'
  expect(localReads).toEqual({})
  await markRead([{ key: 'chat:shared', at: 10 }])
  fail(new Error('Old account failed'))
  await old
  expect(localReads['chat:shared']).toBe(10)
  expect(mocked.notify).not.toHaveBeenCalled()
  state.installationId = 'other'
  expect(localReads).toEqual({})
  await markRead([{ key: 'chat:shared', at: 10 }])
  state.signingOut = true
  expect(localReads).toEqual({})
})

it('an older failed request cannot roll back a newer successful completion', async () => {
  let fail!: (error: Error) => void
  mocked.api.mockImplementationOnce(() => new Promise((_, reject) => fail = reject))
  const old = markRead([{ key: 'chat:a', at: 10 }])
  await markRead([{ key: 'chat:a', at: 20 }])
  fail(new Error('Old request failed'))
  await old
  expect(localReads['chat:a']).toBe(20)
})
