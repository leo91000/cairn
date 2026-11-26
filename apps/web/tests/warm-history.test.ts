import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import { state } from '../src/api'
import {
  cacheScope,
  clearHistoryCache,
  readHistory,
  writeHistory,
} from '../src/history-cache'
import { warmHistory } from '../src/use-live-run'

vi.mock('../src/api', () => ({
  api: vi.fn(),
  state: {
    authenticated: true,
    signingOut: false,
    csrf: 'test-session',
    installationId: '',
  },
  ApiError: class extends Error {},
}))
vi.mock('../src/live-connection', () => ({
  liveConnection: vi.fn(() => ({ close: vi.fn(), reconnect: vi.fn() })),
}))
vi.mock('../src/history-cache', async original => ({
  ...await original<typeof import('../src/history-cache')>(),
  readHistory: vi.fn((await original<typeof import('../src/history-cache')>()).readHistory),
}))

afterEach(async () => {
  Object.assign(state, { authenticated: true, signingOut: false })
  await clearHistoryCache()
  vi.mocked(readHistory).mockClear()
})

it('reads a conversation from storage only when the page does not hold it', async () => {
  await warmHistory('/chats/a/stream')
  const scope = await cacheScope('test-session')
  expect(readHistory).toHaveBeenCalledWith(scope, '/chats/a/stream')

  vi.mocked(readHistory).mockClear()
  await writeHistory(scope, '/chats/b/stream', {
    version: 2,
    cursor: 0,
    history: 'v2:r:1',
    state: { run: null, chat: null, artifacts: [] },
    entries: [],
    events: [],
    savedAt: Date.now(),
  })
  await warmHistory('/chats/b/stream')
  expect(readHistory).not.toHaveBeenCalled()
})

it('reads nothing once signed out', async () => {
  Object.assign(state, { signingOut: true })
  await warmHistory('/chats/a/stream')
  expect(readHistory).not.toHaveBeenCalled()
})
