import type { ChatView } from '../../../packages/contracts/chats'
import type { ChatChanges, LiveState } from '../../../packages/contracts/live'

/** The conversation's run is the state's own `run`: compact streams and saved copies omit it. */
export function withChatRun(state: LiveState): LiveState {
  if (!state.chat || state.chat.run !== undefined)
    return state
  return { ...state, chat: { ...state.chat, run: state.run } }
}

export function applyChatChanges(chats: ChatView[], changes: ChatChanges): ChatView[] {
  const removed = new Set(changes.removed)
  const upserts = new Map(changes.upserts.map(chat => [chat.id, chat]))
  const kept = chats.filter(chat => !removed.has(chat.id))
  const known = new Map(kept.map(chat => [chat.id, chat]))
  const order = changes.order
    ?? [...changes.upserts.filter(chat => !known.has(chat.id)).map(chat => chat.id), ...kept.map(chat => chat.id)]
  return order.flatMap((id) => {
    const chat = upserts.get(id) ?? known.get(id)
    return chat ? [chat] : []
  })
}

/**
 * The state after one stream batch. A compact batch sends its state without an unchanged
 * list, and list changes on their own; both apply to the list received before.
 */
export function nextLiveState(previous: LiveState | undefined, state: LiveState | undefined, changes?: ChatChanges): LiveState | undefined {
  let next = state ? withChatRun(state) : previous
  if (next && state && !state.chats && previous?.chats)
    next = { ...next, chats: previous.chats }
  if (next?.chats && changes)
    next = { ...next, chats: applyChatChanges(next.chats, changes) }
  return next
}
