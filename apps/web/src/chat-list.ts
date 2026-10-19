import type { ChatView } from '../../../packages/contracts/chats'
import { ref, watch } from 'vue'
import { state } from './api'
import { useLiveRun } from './use-live-run'

// Active conversations known to the shell: the Fil, search and badges.
export const chatList = ref<ChatView[]>([])
// A conversation's stream carries the list, which then needs no stream of its own: each
// tab keeps one live stream within its account's stream capacity.
export const chatListCarried = ref(false)
let known = false

/** Shows a list from a stream; a saved one only until the server's list arrives. */
export function publishChats(chats: ChatView[], options: { saved?: boolean } = {}) {
  if (options.saved && known)
    return
  chatList.value = chats
  known = true
}

/**
 * Follows the conversation list for as long as the shell is open, as Android does: on its
 * own stream, unless the open conversation's stream carries it.
 */
export function useChatList() {
  const live = useLiveRun(() => chatListCarried.value ? undefined : '/chats/stream')
  watch(live.snapshot, (value) => {
    if (value?.chats)
      publishChats(value.chats, { saved: live.restored.value })
  })
  watch(() => state.authenticated, (signedIn) => {
    if (signedIn)
      return
    chatList.value = []
    known = false
  })
  return chatList
}
