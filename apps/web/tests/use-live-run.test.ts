import type { EffectScope } from 'vue'
import type { CachedHistory } from '../src/history-cache'
import {
  afterEach,
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { effectScope, ref } from 'vue'
import { api } from '../src/api'
import {
  cacheScope,
  clearHistoryCache,
  peekHistory,
  writeHistory,
} from '../src/history-cache'
import { liveConnection } from '../src/live-connection'
import { useLiveRun } from '../src/use-live-run'

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

let scopes: EffectScope[] = []

beforeEach(() => {
  vi.stubGlobal('window', new EventTarget())
  vi.stubGlobal('document', Object.assign(new EventTarget(), { hidden: false }))
})

afterEach(async () => {
  scopes.forEach(scope => scope.stop())
  scopes = []
  await clearHistoryCache()
  vi.clearAllMocks()
  vi.unstubAllGlobals()
})

it.each(['navigation', 'hidden', 'dispose'] as const)('retains older history when an outdated copy is saved on %s before receiving a batch', async (reason) => {
  const scope = await cacheScope('test-session')
  const stream = '/chats/a/stream'
  const saved: CachedHistory = {
    version: 1,
    cursor: 20,
    history: 'v1:r:1',
    oldest: 20,
    hasOlder: true,
    state: { run: null, chat: null, artifacts: [] },
    events: [{
      id: 20,
      runId: 'r',
      createdAt: 1,
      type: 'output',
      text: 'Recent message',
    }],
    savedAt: Date.now(),
    position: { top: 10, follow: false },
  }
  await writeHistory(scope, stream, saved)

  const path = ref(stream)
  const firstScope = effectScope()
  scopes.push(firstScope)
  const first = firstScope.run(() => useLiveRun(() => path.value, { current: () => false }))!
  expect(first.snapshot.value).toBeUndefined()

  if (reason === 'navigation') {
    path.value = '/chats/b/stream'
  }
  else if (reason === 'hidden') {
    Object.assign(document, { hidden: true })
    document.dispatchEvent(new Event('visibilitychange'))
  }
  else {
    firstScope.stop()
  }

  expect(peekHistory(scope, stream)).toMatchObject({ hasOlder: true, oldest: 20, cursor: 20 })
  firstScope.stop()

  const nextScope = effectScope()
  scopes.push(nextScope)
  const reopened = nextScope.run(() => useLiveRun(() => stream))!
  // An ordinary resume does not resend the window boundary; the saved flag owns it.
  const accept = vi.mocked(liveConnection).mock.calls.at(-1)![1]
  accept({
    events: [],
    reset: false,
    more: false,
    history: saved.history,
  }, 20)
  expect(reopened.hasOlder.value).toBe(true)
  vi.mocked(api).mockResolvedValueOnce({
    events: [],
    oldest: 1,
    hasOlder: false,
    history: saved.history,
  })
  await reopened.loadOlder()
  expect(api).toHaveBeenCalledWith('/chats/a/history?before=20&history=v1%3Ar%3A1')
  expect(reopened.hasOlder.value).toBe(false)
})
