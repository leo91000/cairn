import type { ChatView } from '../../../packages/contracts/chats'
import type { LiveState } from '../../../packages/contracts/live'
import { expect, it } from 'vitest'
import { applyChatChanges, nextLiveState } from '../src/live-state'

const chat = (id: string, title = id) => ({ id, title }) as unknown as ChatView
const ids = (chats: ChatView[] | undefined) => chats?.map(chat => chat.id)

it('updates conversations in place, follows a new order and drops removed ones', () => {
  const list = [chat('a'), chat('b'), chat('c')]
  const renamed = applyChatChanges(list, { upserts: [chat('b', 'Renamed')], removed: [] })
  expect(ids(renamed)).toEqual(['a', 'b', 'c'])
  expect(renamed[1]!.title).toBe('Renamed')
  expect(renamed[0]).toBe(list[0])

  expect(ids(applyChatChanges(list, { upserts: [chat('c')], removed: [], order: ['c', 'a', 'b'] }))).toEqual(['c', 'a', 'b'])
  expect(ids(applyChatChanges(list, { upserts: [], removed: ['b'] }))).toEqual(['a', 'c'])
  expect(ids(applyChatChanges(list, { upserts: [chat('d')], removed: ['a'], order: ['b', 'd', 'c'] }))).toEqual(['b', 'd', 'c'])
})

it('keeps the list a compact state omits and restores the chat run', () => {
  const run = { id: 'run', status: 'succeeded' }
  const first = nextLiveState(undefined, {
    run,
    chat: { id: 'a', messages: [] },
    chats: [chat('a'), chat('b')],
    artifacts: [],
  } as unknown as LiveState)!
  expect(first.chat!.run).toBe(run)

  // The chat changed; the list did not.
  const paused = nextLiveState(first, { ...first, chats: undefined, chat: { id: 'a', messages: [], paused: true } } as unknown as LiveState)!
  expect(ids(paused.chats)).toEqual(['a', 'b'])
  expect(paused.chat!.paused).toBe(true)
  expect(paused.chat!.run).toBe(run)

  // Only the list changed.
  const moved = nextLiveState(paused, undefined, { upserts: [chat('b', 'Later')], removed: [], order: ['b', 'a'] })!
  expect(ids(moved.chats)).toEqual(['b', 'a'])
  expect(moved.chat).toBe(paused.chat)

  // A full state still replaces the list.
  expect(ids(nextLiveState(moved, { ...first, chats: [chat('c')] })!.chats)).toEqual(['c'])
})
