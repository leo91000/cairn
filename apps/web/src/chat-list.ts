import type { ChatView } from '../../../packages/contracts/chats'
import { onScopeDispose, ref, watch } from 'vue'
import { state } from './api'
import { cacheScope, readChatList, writeChatList } from './history-cache'
import { cacheSession, useLiveRun } from './use-live-run'

// Active conversations known to the shell: the Fil, search and badges.
export const chatList = ref<ChatView[]>([])
// A conversation's stream carries the list, which then needs no stream of its own: each
// tab keeps one live stream within its account's stream capacity.
export const chatListCarried = ref(false)
export const chatPlanEnabled = ref(false)
let known = false
/** The server's list waiting to be saved: saving serializes it, at most every few seconds. */
let unsaved: { session: string, chats: ChatView[], planTurnsEnabled: boolean } | undefined
let saveTimer: ReturnType<typeof setTimeout> | undefined
const SAVE_DELAY = 2000

function saveList() {
  saveTimer = undefined
  const list = unsaved
  unsaved = undefined
  if (!list || !state.authenticated || state.signingOut || list.session !== cacheSession())
    return
  void cacheScope(list.session)
    .then(scope => writeChatList(scope, { chats: list.chats, planTurnsEnabled: list.planTurnsEnabled, savedAt: Date.now() }))
    .catch(() => {})
}

/** Shows a list from a stream; a saved one only until the server's list arrives. */
export function publishChats(chats: ChatView[], options: { saved?: boolean, planTurnsEnabled?: boolean } = {}) {
  if (options.saved && known)
    return
  chatList.value = chats
  chatPlanEnabled.value = options.planTurnsEnabled === true
  known = true
  if (options.saved)
    return
  unsaved = { session: cacheSession(), chats, planTurnsEnabled: options.planTurnsEnabled === true }
  saveTimer ??= setTimeout(saveList, SAVE_DELAY)
}

// The list saved last shows at once; any list from a stream replaces it.
async function restoreList() {
  const session = cacheSession()
  const saved = await readChatList(await cacheScope(session)).catch(() => undefined)
  if (saved && session === cacheSession() && state.authenticated && !state.signingOut)
    publishChats(saved.chats, { saved: true, planTurnsEnabled: saved.planTurnsEnabled })
}

/**
 * Follows the conversation list for as long as the shell is open, as Android does: on its
 * own stream, unless the open conversation's stream carries it.
 */
export function useChatList() {
  const live = useLiveRun(() => chatListCarried.value ? undefined : '/chats/stream')
  watch(live.snapshot, (value) => {
    if (value?.chats)
      publishChats(value.chats, { saved: live.restored.value, planTurnsEnabled: value.planTurnsEnabled })
  }, { immediate: true })
  watch(() => state.authenticated && !state.signingOut && state.csrf ? cacheSession() : undefined, (session) => {
    if (session)
      void restoreList()
  }, { immediate: true })
  watch(() => state.authenticated, (signedIn) => {
    if (signedIn)
      return
    chatList.value = []
    chatPlanEnabled.value = false
    known = false
    unsaved = undefined
    clearTimeout(saveTimer)
    saveTimer = undefined
  })
  const leaving = () => {
    if (saveTimer === undefined)
      return
    clearTimeout(saveTimer)
    saveList()
  }

  window.addEventListener('pagehide', leaving)
  onScopeDispose(() => window.removeEventListener('pagehide', leaving))
  watch(() => state.installationId, () => {
    chatPlanEnabled.value = false
  }, { immediate: true, flush: 'sync' })
  return chatList
}
