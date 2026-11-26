import type { ChatView } from '../../../packages/contracts/chats'
import { onScopeDispose, ref, watch } from 'vue'
import { state } from './api'
import {
  cacheScope,
  forgetHistory,
  readChatList,
  removeRunLogs,
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
let lastSaved: { chats: ChatView[], planTurnsEnabled: boolean } | undefined
// The server shares a list for up to a quarter second; this leaves ample margin.
const RECENT = 10_000
const SAVE_DELAY = 2000

function saveList() {
  saveTimer = undefined
  const list = unsaved
  unsaved = undefined
  lastSaved = list
  if (!list || !state.authenticated || state.signingOut || list.session !== cacheSession())
    return
  void cacheScope(list.session)
    .then(scope => writeChatList(scope, { chats: list.chats, planTurnsEnabled: list.planTurnsEnabled, savedAt: Date.now() }))
    .catch(() => {})
}

// A conversation that leaves the server's list was deleted or is no longer shared with this
// person: its saved copies go too, under the slug and the identifier it may be opened by, and
// the saved run logs, among which its earlier runs cannot be told apart.
function forgetRemoved(previous: ChatView[], chats: ChatView[], spareRecent = false) {
  const listed = new Set(chats.map(chat => chat.id))
  // A list another tab saved may hold a conversation created after the server list read here:
  // only one older than the newest the server lists, by the server's clock, is gone then. With
  // nothing listed, every saved conversation is gone.
  const newest = chats.length ? Math.max(...chats.map(chat => chat.createdAt)) : Number.POSITIVE_INFINITY
  const removed = previous.filter(chat => !listed.has(chat.id) && (!spareRecent || chat.createdAt < newest - RECENT))
  if (!removed.length)
    return
  void cacheScope(cacheSession()).then((scope) => {
    for (const chat of removed) {
      for (const key of new Set([chat.slug || chat.id, chat.id]))
        void forgetHistory(scope, `/chats/${key}/stream`)
    }

    void removeRunLogs(scope)
  }).catch(() => {})
}

/** Shows a list from a stream; a saved one only until the server's list arrives. */
export function publishChats(chats: ChatView[], options: { saved?: boolean, planTurnsEnabled?: boolean } = {}) {
  if (options.saved && known)
    return
  // The first server list may follow a saved one, perhaps saved by another tab.
  if (!options.saved && known)
    forgetRemoved(chatList.value, chats, !served)
  served ||= !options.saved
  chatList.value = chats
  chatPlanEnabled.value = options.planTurnsEnabled === true
  known = true
  // A snapshot that keeps the same list and Plan setting saves nothing again.
  const planTurnsEnabled = options.planTurnsEnabled === true
  const pending = unsaved ?? lastSaved
  if (options.saved || (chats === pending?.chats && planTurnsEnabled === pending.planTurnsEnabled))
    return
  unsaved = { session: cacheSession(), chats, planTurnsEnabled }
  saveTimer ??= setTimeout(saveList, SAVE_DELAY)
}

function reset() {
  chatList.value = []
  chatPlanEnabled.value = false
  known = false
  served = false
  unsaved = undefined
  lastSaved = undefined
  clearTimeout(saveTimer)
  saveTimer = undefined
}

// The list saved last shows at once; any list from a stream replaces it.
async function restoreList() {
  const session = cacheSession()
  const saved = await readChatList(await cacheScope(session)).catch(() => undefined)
  if (!saved || session !== cacheSession() || !state.authenticated || state.signingOut)
    return
  // The server's list may have arrived first: what left it since the save goes too.
  if (served)
    forgetRemoved(saved.chats, chatList.value, true)
  else
    publishChats(saved.chats, { saved: true, planTurnsEnabled: saved.planTurnsEnabled })
}

/**
 * Follows the conversation list for as long as the shell is open, as Android does: on its
 * own stream, unless the open conversation's stream carries it.
 */
export function useChatList() {
  // The list is saved apart, so this stream saves nothing of its own.
  const live = useLiveRun(() => chatListCarried.value ? undefined : '/chats/stream', { save: false })
  watch(live.snapshot, (value) => {
    if (value?.chats)
      publishChats(value.chats, { saved: live.restored.value, planTurnsEnabled: value.planTurnsEnabled })
  }, { immediate: true })
  watch(() => state.authenticated && !state.signingOut && state.csrf ? cacheSession() : undefined, (session, previous) => {
    // Another installation or session starts from its own lists: comparing them with the
    // previous one's would forget conversations that were never removed.
    if (previous && session !== previous)
      reset()
    if (session)
      void restoreList()
  }, { immediate: true })
  watch(() => state.authenticated, (signedIn) => {
    if (!signedIn)
      reset()
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
