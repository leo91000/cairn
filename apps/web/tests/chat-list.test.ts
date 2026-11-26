import type { ChatView } from '../../../packages/contracts/chats'
import type { CachedHistory } from '../src/history-cache'
import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import {
  cacheScope,
  clearHistoryCache,
  peekHistory,
  writeHistory,
} from '../src/history-cache'

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

afterEach(async () => {
  await clearHistoryCache()
})

function saved(): CachedHistory {
  return {
    version: 2,
    cursor: 0,
    history: 'v2:r:1',
    state: { run: null, chat: null, artifacts: [] },
    entries: [],
    events: [],
    savedAt: Date.now(),
  }
}

it('forgets the saved copies of conversations that leave the server list', async () => {
  const { publishChats } = await import('../src/chat-list')
  const scope = await cacheScope('test-session')
  for (const path of ['/chats/gone-slug/stream', '/chats/gone/stream', '/runs/gone-run/stream', '/chats/kept/stream'])
    await writeHistory(scope, path, saved())

  const gone = {
    id: 'gone',
    slug: 'gone-slug',
    runId: 'gone-run',
    createdAt: 1,
  } as ChatView
  const kept = { id: 'kept', slug: '', createdAt: 1 } as ChatView
  // The server's lists only move forward: even the newest conversation is gone once unlisted.
  const fresh = { id: 'fresh', slug: '', createdAt: Date.now() } as ChatView
  await writeHistory(scope, '/chats/fresh/stream', saved())
  publishChats([gone, kept, fresh])
  // A saved list never removes anything: only the server's list does.
  publishChats([kept], { saved: true })
  expect(peekHistory(scope, '/chats/gone-slug/stream')).toBeDefined()

  publishChats([kept])
  await vi.waitFor(() => expect(peekHistory(scope, '/chats/gone-slug/stream')).toBeUndefined())
  expect(peekHistory(scope, '/chats/gone/stream')).toBeUndefined()
  // Run logs do not name their conversation: every saved one goes.
  expect(peekHistory(scope, '/runs/gone-run/stream')).toBeUndefined()
  expect(peekHistory(scope, '/chats/kept/stream')).toBeDefined()
  expect(peekHistory(scope, '/chats/fresh/stream')).toBeUndefined()
})

it('forgets what left the list since it was saved when the server list arrives first', async () => {
  const { publishChats, useChatList } = await import('../src/chat-list')
  const { effectScope } = await import('vue')
  const { writeChatList } = await import('../src/history-cache')
  vi.stubGlobal('window', new EventTarget())
  vi.stubGlobal('document', Object.assign(new EventTarget(), { hidden: false }))
  const scope = await cacheScope('test-session')
  await writeHistory(scope, '/chats/gone/stream', saved())
  await writeHistory(scope, '/chats/newer/stream', saved())
  // Another tab saved a list holding a conversation created after the server list read here.
  await writeChatList(scope, { chats: [{ id: 'gone', slug: '', createdAt: 1 }, { id: 'kept', slug: '', createdAt: 20_000 }, { id: 'newer', slug: '', createdAt: 25_000 }] as ChatView[], planTurnsEnabled: false, savedAt: Date.now() })

  publishChats([{ id: 'kept', slug: '', createdAt: 20_000 } as ChatView])
  const effect = effectScope()
  const list = effect.run(() => useChatList())!
  await vi.waitFor(() => expect(peekHistory(scope, '/chats/gone/stream')).toBeUndefined())
  expect(peekHistory(scope, '/chats/newer/stream')).toBeDefined()
  expect(list.value.map(chat => chat.id)).toEqual(['kept'])
  effect.stop()
  vi.unstubAllGlobals()
})

it('spares recent conversations of a saved list shown before the server list, unless nothing is listed', async () => {
  vi.resetModules()
  const { publishChats } = await import('../src/chat-list')
  const history = await import('../src/history-cache')
  const scope = await history.cacheScope('test-session')
  for (const path of ['/chats/gone/stream', '/chats/newer/stream'])
    await history.writeHistory(scope, path, saved())
  const gone = { id: 'gone', slug: '', createdAt: 1 } as ChatView
  const kept = { id: 'kept', slug: '', createdAt: 20_000 } as ChatView
  const newer = { id: 'newer', slug: '', createdAt: 25_000 } as ChatView

  publishChats([gone, kept, newer], { saved: true })
  publishChats([kept])
  await vi.waitFor(() => expect(history.peekHistory(scope, '/chats/gone/stream')).toBeUndefined())
  expect(history.peekHistory(scope, '/chats/newer/stream')).toBeDefined()

  // The server's own lists only move forward, and an empty one leaves nothing to spare.
  publishChats([kept, newer])
  publishChats([])
  await vi.waitFor(() => expect(history.peekHistory(scope, '/chats/newer/stream')).toBeUndefined())
})

it('saves the list again when only the Plan setting changes', async () => {
  vi.resetModules()
  vi.useFakeTimers()
  const { publishChats } = await import('../src/chat-list')
  const history = await import('../src/history-cache')
  const scope = await history.cacheScope('test-session')
  const chats = [{ id: 'kept', slug: '', createdAt: 1 }] as ChatView[]
  publishChats(chats, { planTurnsEnabled: false })
  await vi.advanceTimersByTimeAsync(2000)
  expect(await history.readChatList(scope)).toMatchObject({ planTurnsEnabled: false })
  publishChats(chats, { planTurnsEnabled: true })
  await vi.advanceTimersByTimeAsync(2000)
  expect(await history.readChatList(scope)).toMatchObject({ planTurnsEnabled: true })
  vi.useRealTimers()
})
