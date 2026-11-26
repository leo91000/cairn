import type { ChatView } from '../../../packages/contracts/chats'
import { expect, it, vi } from 'vitest'
import { effectScope, watch } from 'vue'
import { state } from '../src/api'
import { publishChats, useChatList } from '../src/chat-list'
import {
  cacheScope,
  peekHistory,
  writeChatList,
  writeHistory,
} from '../src/history-cache'
import { cacheSession } from '../src/use-live-run'

vi.mock('../src/api', async () => {
  const { reactive } = await import('vue')
  return {
    api: vi.fn(),
    state: reactive({
      authenticated: true,
      signingOut: false,
      csrf: 'test-session',
      installationId: 'first',
    }),
    ApiError: class extends Error {},
  }
})
vi.mock('../src/live-connection', () => ({
  liveConnection: vi.fn(() => ({ close: vi.fn(), reconnect: vi.fn() })),
}))

it('compares another installation\'s lists only with its own', async () => {
  vi.stubGlobal('window', new EventTarget())
  vi.stubGlobal('document', Object.assign(new EventTarget(), { hidden: false }))
  const effect = effectScope()
  const list = effect.run(() => useChatList())!
  publishChats([{ id: 'a', slug: '', createdAt: 50_000 } as ChatView])
  const shown: string[][] = []
  effect.run(() => watch(list, value => shown.push(value.map(chat => chat.id)), { flush: 'sync' }))

  // The second installation's lists are saved before it opens, so it can restore them.
  const second = await cacheScope('second:test-session')
  await writeHistory(second, '/chats/b/stream', {
    version: 2,
    cursor: 0,
    history: 'v2:r:1',
    state: { run: null, chat: null, artifacts: [] },
    entries: [],
    events: [],
    savedAt: Date.now(),
  })
  await writeChatList(second, { chats: [{ id: 'b', slug: '', createdAt: 1 }] as ChatView[], planTurnsEnabled: false, savedAt: Date.now() })
  Object.assign(state, { installationId: 'second' })
  expect(cacheSession()).toBe('second:test-session')
  await vi.waitFor(() => expect(list.value.map(chat => chat.id)).toEqual(['b']))
  // The first installation's list left at once; the second's saved list replaced it.
  expect(shown).toEqual([[], ['b']])
  publishChats([{ id: 'b', slug: '', createdAt: 1 } as ChatView])
  await new Promise(resolve => setTimeout(resolve, 10))
  expect(peekHistory(second, '/chats/b/stream')).toBeDefined()
  effect.stop()
  vi.unstubAllGlobals()
})
