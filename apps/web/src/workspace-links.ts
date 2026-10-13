import type { Chat } from '../../../packages/contracts/chats'

export function chatPath(chat: Pick<Chat, 'id' | 'slug'>) {
  return `/chats/${encodeURIComponent(chat.slug || chat.id)}`
}
