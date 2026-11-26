import type { ChatView } from '../../../packages/contracts/chats'
import { expect, it, vi } from 'vitest'
import { effectScope, nextTick } from 'vue'
import { state } from '../src/api'
import { publishChats, useChatList } from '../src/chat-list'
import {
  cacheScope,
  peekHistory,
  writeChatList,
  writeHistory,
} from '../src/history-cache'

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

  const second = await cacheScope(`second:${state.csrf}`)
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
  await nextTick()
  // The first installation's list leaves at once; the second's saved list replaces it.
  expect(list.value).toEqual([])
  await vi.waitFor(() => expect(list.value.map(chat => chat.id)).toEqual(['b']))
  publishChats([{ id: 'b', slug: '', createdAt: 1 } as ChatView])
  await new Promise(resolve => setTimeout(resolve, 10))
  expect(peekHistory(second, '/chats/b/stream')).toBeDefined()
  effect.stop()
  vi.unstubAllGlobals()
})
