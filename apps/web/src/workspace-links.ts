import type { Chat } from '../../../packages/contracts/chats'

export function chatPath(chat: Pick<Chat, 'id'> & Partial<Pick<Chat, 'slug'>>) {
  return `/chats/${encodeURIComponent(chat.slug || chat.id)}`
}
