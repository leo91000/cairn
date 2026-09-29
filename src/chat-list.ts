import type { ChatView } from '../shared/chats'
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api, state } from './api'

// Active conversations known to the shell (search, badges). Live views publish their stream
// snapshots here; otherwise the list is refreshed while the page is visible.
export const chatList = ref<ChatView[]>([])
let publishedAt = 0

export function publishChats(chats: ChatView[]) {
  chatList.value = chats
  publishedAt = Date.now()
}

// Only while signed in: a request racing sign-in or sign-out must not reset the session.
export async function refreshChats() {
  if (!state.authenticated || state.signingOut || state.redirecting)
    return
  try {
    chatList.value = await api<ChatView[]>('/chats')
  }
  catch {
    // Keep the last known list while the server reconnects.
  }
}

/**
 * The shell's list for search and badges. Views with a live stream publish it; otherwise it is
 * fetched a moment after the page loads, then refreshed slowly while the page is visible.
 */
export function useChatList() {
  let timer: ReturnType<typeof setInterval> | undefined
  let first: ReturnType<typeof setTimeout> | undefined
  const stale = () => Date.now() - publishedAt > 30000
  const start = () => {
    clearTimeout(first)
    first = setTimeout(() => stale() && refreshChats(), 3000)
  }
  onMounted(() => {
    start()
    timer = setInterval(() => {
      if (!document.hidden && stale())
        void refreshChats()
    }, 30000)
  })
  onBeforeUnmount(() => {
    clearTimeout(first)
    clearInterval(timer)
  })
  watch(() => state.authenticated, (signedIn) => {
    if (signedIn)
      start()
    else
      chatList.value = []
  })
  return chatList
}
