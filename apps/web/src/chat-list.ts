import type { ChatView } from '../../../packages/contracts/chats'
import { onScopeDispose, ref, watch } from 'vue'
import { state } from './api'
import {
  cacheScope,
  readChatList,
  removeHistory,
  writeChatList,
} from './history-cache'
import { cacheSession, useLiveRun } from './use-live-run'

// Active conversations known to the shell: the Fil, search and badges.
export const chatList = ref<ChatView[]>([])
// A conversation's stream carries the list, which then needs no stream of its own: each
// tab keeps one live stream within its account's stream capacity.
export const chatListCarried = ref(false)
export const chatPlanEnabled = ref(false)
let known = false
// The list shown came from a server stream, not from storage.
let served = false
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

// A conversation that leaves the server's list was deleted or is no longer shared with this
// person: its saved copy goes too, under the slug and the identifier it may be opened by.
function forgetRemoved(previous: ChatView[], chats: ChatView[]) {
  const listed = new Set(chats.map(chat => chat.id))
  const removed = previous.filter(chat => !listed.has(chat.id))
  if (!removed.length)
    return
  void cacheScope(cacheSession()).then((scope) => {
    for (const chat of removed) {
      for (const key of new Set([chat.slug || chat.id, chat.id]))
        void removeHistory(scope, `/chats/${key}/stream`)
    }
  }).catch(() => {})
}

/** Shows a list from a stream; a saved one only until the server's list arrives. */
export function publishChats(chats: ChatView[], options: { saved?: boolean, planTurnsEnabled?: boolean } = {}) {
  if (options.saved && known)
    return
  if (!options.saved && known)
    forgetRemoved(chatList.value, chats)
  served ||= !options.saved
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
  if (!saved || session !== cacheSession() || !state.authenticated || state.signingOut)
    return
  // The server's list may have arrived first: what left it since the save goes too.
  if (served)
    forgetRemoved(saved.chats, chatList.value)
  else
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
    served = false
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

  const hidden = () => document.hidden && leaving()

  window.addEventListener('pagehide', leaving)
  document.addEventListener('visibilitychange', hidden)
  onScopeDispose(() => {
    window.removeEventListener('pagehide', leaving)
    document.removeEventListener('visibilitychange', hidden)
  })
  watch(() => state.installationId, () => {
    chatPlanEnabled.value = false
  }, { immediate: true, flush: 'sync' })
  return chatList
}
