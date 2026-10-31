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
import { api, ApiError, state } from '../src/api'
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
  ApiError: class extends Error {
    constructor(message: string, readonly status: number) { super(message) }
  },
}))
vi.mock('../src/live-connection', () => ({
  liveConnection: vi.fn(() => ({ close: vi.fn(), reconnect: vi.fn() })),
}))

let scopes: EffectScope[] = []

beforeEach(() => {
  vi.stubGlobal('window', new EventTarget())
  vi.stubGlobal('document', Object.assign(new EventTarget(), { hidden: false }))
  Object.assign(state, { installationId: '' })
})

afterEach(async () => {
  scopes.forEach(scope => scope.stop())
  scopes = []
  await clearHistoryCache()
  vi.clearAllMocks()
  vi.mocked(api).mockReset()
  vi.unstubAllGlobals()
})

it('shows the installation update transition without declaring an empty conversation synchronized', async () => {
  const scope = effectScope()
  scopes.push(scope)
  const live = scope.run(() => useLiveRun(() => '/chats/updating/stream'))!
  await vi.waitFor(() => expect(liveConnection).toHaveBeenCalled())
  vi.mocked(liveConnection).mock.calls[0]![2]('updating')
  expect(live.connectionNotice.value).toBe('This installation must finish updating. Retrying…')
  expect(live.synced.value).toBe(false)
  expect(live.error.value).toBe('')
})

it('releases list delivery when a conversation is terminally unavailable', async () => {
  const scope = effectScope()
  scopes.push(scope)
  const live = scope.run(() => useLiveRun(() => '/chats/missing/stream', { list: true }))!
  expect(live.carriesList.value).toBe(true)
  await vi.waitFor(() => expect(liveConnection).toHaveBeenCalled())

  vi.mocked(api).mockRejectedValueOnce(new ApiError('Chat not found', 404))
  vi.mocked(liveConnection).mock.calls[0]![2]('reconnecting')
  await vi.waitFor(() => expect(live.error.value).toBe('Chat not found'))
  expect(live.carriesList.value).toBe(false)
  expect(vi.mocked(liveConnection).mock.results[0]!.value.close).toHaveBeenCalled()
})

it('loads a referenced card before exposing a completed stream batch', async () => {
  const scope = effectScope()
  scopes.push(scope)
  const live = scope.run(() => useLiveRun(() => '/runs/r/stream'))!
  await vi.waitFor(() => expect(liveConnection).toHaveBeenCalled())
  const card = {
    id: 'message:1:a',
    runId: 'r',
    position: 1,
    revision: 2,
    source: 2,
    turn: 1,
    attempt: 0,
    createdAt: 1,
    updatedAt: 2,
    content: {
      kind: 'agentMessage' as const,
      itemId: 'a',
      text: 'full answer',
      state: 'done' as const,
    },
  }
  const data = JSON.stringify(card)
  let resolve: ((value: unknown) => void) | undefined
  vi.mocked(api).mockImplementationOnce(() => new Promise((done) => {
    resolve = done
  }))
  const accept = vi.mocked(liveConnection).mock.calls.at(-1)![1]
  const pending = accept({
    events: [],
    entries: [],
    reset: false,
    more: false,
    history: 'v2:r:1',
    largeEntries: [{ snapshot: { ...card, content: { ...card.content, itemId: '', text: '' } } }],
  }, 2)
  expect(live.timeline.value).toHaveLength(0)
  resolve?.({
    entryId: card.id,
    runId: card.runId,
    offset: 0,
    totalBytes: new TextEncoder().encode(data).length,
    sha256: 'a'.repeat(64),
    data,
    nextOffset: null,
  })
  await pending
  expect(live.timeline.value).toMatchObject([{ id: card.id, content: { text: 'full answer' } }])
})

it.each(['navigation', 'hidden', 'dispose'] as const)('retains older history when an outdated copy is saved on %s before receiving a batch', async (reason) => {
  const scope = await cacheScope('test-session')
  const stream = '/chats/a/stream'
  const saved: CachedHistory = {
    version: 2,
    cursor: 20,
    history: 'v2:r:1',
    oldest: 20,
    hasOlder: true,
    state: { run: null, chat: null, artifacts: [] },
    entries: [],
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
  await accept({
    events: [],
    reset: false,
    more: false,
    history: saved.history,
  }, 20)
  expect(reopened.hasOlder.value).toBe(true)
  vi.mocked(api).mockResolvedValueOnce({
    events: [],
    entries: [],
    oldest: 1,
    hasOlder: false,
    history: saved.history,
  })
  await reopened.loadOlder()
  expect(api).toHaveBeenCalledWith('/chats/a/history?before=20&history=v2%3Ar%3A1')
  expect(reopened.hasOlder.value).toBe(false)
})
