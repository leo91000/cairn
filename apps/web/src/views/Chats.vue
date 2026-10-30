<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type {
  Chat,
  ChatAttachment,
  ChatDetail,
  ChatMessage,
} from '../../../../packages/contracts/chats'
import type { DraftState } from '../attachments'
import type { LiveWait } from '../chat-wait'
import type { CachedHistory } from '../history-cache'
import {
  computed,
  inject,
  nextTick,
  onBeforeUnmount,
  provide,
  ref,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { providers } from '../../../../packages/contracts/accounts'
import { latestArtifacts } from '../../../../packages/contracts/artifacts'
import { MAIN_AGENT_ID } from '../../../../packages/contracts/constants'
import { compactChat } from '../../../../packages/contracts/generated/compactChat'
import { createChat } from '../../../../packages/contracts/generated/createChat'
import { deleteChatMessage } from '../../../../packages/contracts/generated/deleteChatMessage'
import { editChatMessage } from '../../../../packages/contracts/generated/editChatMessage'
import { restartChatSession } from '../../../../packages/contracts/generated/restartChatSession'
import { restoreChat } from '../../../../packages/contracts/generated/restoreChat'
import { resumeRun } from '../../../../packages/contracts/generated/resume-run'
import { sendChatMessage } from '../../../../packages/contracts/generated/sendChatMessage'
import { setChatPaused } from '../../../../packages/contracts/generated/setChatPaused'
import { stopChat } from '../../../../packages/contracts/generated/stopChat'
import { failureReason, sameReason, stopEcho } from '../activity'
import {
  api,
  apiUpload,
  refreshSkills,
  state,
} from '../api'
import { reuseAttachmentKey } from '../attachment-media'
import {
  isImage,
  MAX_ATTACHMENTS,
  MAX_MESSAGE_SIZE,
  validateSelection,
} from '../attachments'
import { chatDelivery, chatWaitNotice, scheduledWakeup } from '../chat-delivery'
import { chatListCarried, chatList as chats, publishChats } from '../chat-list'
import { chatQuota, isQuotaMessage, quotaHiddenEvents } from '../chat-quota'
import { liveWakeup, waitStatus } from '../chat-wait'
import ActivityFeed from '../components/ActivityFeed.vue'
import AgentAvatar from '../components/AgentAvatar.vue'
import AssistantPicker from '../components/AssistantPicker.vue'
import AttachMenu from '../components/AttachMenu.vue'
import ChatQuestions from '../components/ChatQuestions.vue'
import ChatSwitcher from '../components/ChatSwitcher.vue'
import ComposerAttachments from '../components/ComposerAttachments.vue'
import ConnectionSheet from '../components/ConnectionSheet.vue'
import ConversationDeletion from '../components/ConversationDeletion.vue'
import Icon from '../components/Icon.vue'
import MessageAttachments from '../components/MessageAttachments.vue'
import Modal from '../components/Modal.vue'
import NodeExecution from '../components/NodeExecution.vue'
import PlanBar from '../components/PlanBar.vue'
import QueueStrip from '../components/QueueStrip.vue'
import RecoveryNotice from '../components/RecoveryNotice.vue'
import SkillTextarea from '../components/SkillTextarea.vue'
import SubagentBar from '../components/SubagentBar.vue'
import SubagentPanel from '../components/SubagentPanel.vue'
import ThemeControl from '../components/ThemeControl.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import UiNotice from '../components/UiNotice.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import { contextSummary } from '../context-usage'
import {
  ArrowDown,
  ArrowUp,
  ArrowUpRight,
  Bell,
  Bot,
  Check,
  ChevronDown,
  ChevronLeft,
  CornerDownRight,
  FileText,
  FolderGit2,
  Hourglass,
  KeyRound,
  Layers,
  LoaderCircle,
  Maximize2,
  Menu,
  MoreHorizontal,
  NotebookPen,
  Paperclip,
  Plug,
  Plus,
  Search,
  Shield,
  Trash2,
  X,
  Zap,
} from '../icons'
import { connectionTone, installationKey, routeLabel } from '../installation-context'
import { modelCatalogs } from '../models'
import { useReadWhileOpen } from '../reads'
import { useRetryState } from '../run-retry'
import { exclusiveSuspensionLabel, isExclusivelySuspended } from '../run-status'
import { backgroundWait } from '../signal'
import { chatSkills } from '../skill-mentions'
import { delegatedChatsKey, openSubagentKey, subagentState } from '../subagents'
import { iconButton } from '../ui'
import { useLiveRun } from '../use-live-run'
import { workspaceActionsKey } from '../workspace-actions'
import { chatPath } from '../workspace-links'

const router = useRouter()
const route = useRoute()
const detail = ref<ChatDetail | null>(null)
const recoverable = computed(() => detail.value?.lifecycle === 'trash' && (detail.value?.purgeAt ?? 0) > Date.now())
const inactive = computed(() => !!detail.value?.lifecycle && detail.value.lifecycle !== 'active')
const chatId = computed(() => typeof route.params.id === 'string' ? route.params.id : undefined)
const selectedChatId = computed(() => {
  const key = chatId.value
  if (!key)
    return undefined
  const known = chats.value.find(chat => chat.id === key || chat.slug === key)
  if (known)
    return known.id
  if (detail.value && (detail.value.id === key || detail.value.slug === key))
    return detail.value.id
  return key
})
// While a conversation opens, the header names it from the Fil.
const listed = computed(() => chats.value.find(chat => chat.id === selectedChatId.value))
const heading = computed(() => detail.value ?? listed.value)

// A saved conversation is current when the Fil shows nothing new since it was saved:
// the same finished turn, no work in progress and no later change.
function savedIsCurrent(saved: CachedHistory) {
  const chat = saved.state.chat
  const known = chat && chats.value.find(item => item.id === chat.id)
  if (!chat || !known)
    return true
  const run = saved.state.run
  return known.runId === (run?.id ?? null)
    && known.status === (run?.status ?? 'idle')
    && known.status !== 'queued' && known.status !== 'running'
    && (known.finishedAt ?? null) === (run?.finishedAt ?? null)
    && known.updatedAt <= chat.updatedAt
}

// The view shows the selected conversation only. A current saved copy shows at once;
// otherwise a loader stands in until its first snapshot, which shows in one piece.
// A conversation the Fil knows opens under the slug the Fil links to, so a link by
// identifier and one by slug share one saved copy.
const streamPath = ref<string>()
watch(chatId, (key) => {
  const known = key && chats.value.find(chat => chat.id === key || chat.slug === key)
  streamPath.value = key ? `/chats/${known ? known.slug || known.id : key}/stream` : undefined
}, { immediate: true, flush: 'sync' })
const live = useLiveRun(() => streamPath.value, {
  current: savedIsCurrent,
  list: true,
  // Also saved under its slug when a link by identifier opened it: the Fil links to slugs.
  savedAs: state => state.chat ? `/chats/${state.chat.slug || state.chat.id}/stream` : undefined,
})
// Its stream carries the list while available; a failed open releases it to the shell,
// so the Fil keeps working even when the conversation cannot.
watch(live.carriesList, value => chatListCarried.value = value, { immediate: true, flush: 'sync' })
onBeforeUnmount(() => chatListCarried.value = false)
const { events, connectionNotice, catchingUp } = live
const deliverables = computed(() => live.snapshot.value?.artifacts ?? [])
const draft = ref('')
const model = ref('')
const reasoning = ref('')
const speed = ref('')
// Names queued messages' speeds; the composer's picker keeps this catalog current.
const codexModels = computed(() => modelCatalogs.codex.catalog.models)
const provider = ref<'' | Provider>('')
// The next messages are plan turns until the user turns it off or implements a plan.
const planning = ref(false)
const history = ref(false)
const deletingConversation = ref<ChatDetail>()
const detailsOpen = ref(false)
const detailsButton = ref<HTMLButtonElement>()
const activity = ref<InstanceType<typeof ActivityFeed>>()
const textarea = ref<InstanceType<typeof SkillTextarea>>()
const workspaceActions = inject(workspaceActionsKey)
const installation = inject(installationKey, null)
const currentInstallation = computed(() => installation?.current.value ?? null)
const connectionOpen = ref(false)
const fileCount = computed(() => latestArtifacts(deliverables.value).length)

// A subagent's full activity opens beside the conversation on wide screens, in a drawer otherwise.
// The current request's subagents stay in sight beside the composer while one of them works.
const openedSubagent = ref<string | null>(null)
let subagentOpener: HTMLElement | null = null
const wideQuery = window.matchMedia('(width > 1200px)')
const wide = ref(wideQuery.matches)
const onWide = () => wide.value = wideQuery.matches
wideQuery.addEventListener('change', onWide)
onBeforeUnmount(() => wideQuery.removeEventListener('change', onWide))

const currentSubagents = computed(() => activity.value?.subagents ?? [])

function openSubagent(id: string, opener?: HTMLElement) {
  // A delegated task is a conversation of its own: open it rather than a panel.
  const delegation = currentSubagents.value.find(agent => agent.id === id)?.subagent?.delegation
  if (delegation) {
    void router.push(chatPath({ id: delegation.chatId }))
    return
  }

  subagentOpener = opener ?? (document.activeElement instanceof HTMLElement ? document.activeElement : null)
  openedSubagent.value = id
}

async function closeSubagent() {
  openedSubagent.value = null
  await nextTick()
  if (subagentOpener?.isConnected && subagentOpener.getClientRects().length)
    subagentOpener.focus()
  else
    textarea.value?.focus()
  subagentOpener = null
}

provide(openSubagentKey, openSubagent)
const delegatedChats = computed(() => new Map(chats.value.map(chat => [chat.id, chat])))
provide(delegatedChatsKey, delegatedChats)
const runActive = computed(() => detail.value?.run?.status === 'running')
const subagentTeam = computed(() => activity.value?.teams.find(team => team.some(agent => agent.id === openedSubagent.value)))
const subagentBar = computed(() => currentSubagents.value.some(agent => subagentState(agent, runActive.value, delegatedChats.value) === 'running'))

async function detailAction(action: () => void) {
  detailsOpen.value = false
  // Restore focus to the toolbar before opening another modal or navigation.
  await nextTick()
  action()
}

const historyButton = ref<HTMLButtonElement>()
const tabletHistoryButton = ref<HTMLButtonElement>()

// Desktop lists conversations in the Fil column; narrower layouts open them from the header.
function switcherAnchor() {
  return [historyButton.value, tabletHistoryButton.value, detailsButton.value].find(button => button?.getClientRects().length)
}

const error = ref('')
const dismissedError = ref(false)
const quota = computed(() => chatQuota(detail.value?.run, events.value))
const visibleError = computed(() => {
  if (error.value)
    return error.value
  const chatError = detail.value?.error
  if (quota.value && (!chatError || isQuotaMessage(chatError) || chatError === quota.value.message))
    return ''
  const failedRun = detail.value?.run?.status === 'failed' ? detail.value.run : null
  return chatError || (failedRun ? failedRun.error || 'The response stopped before finishing. Resume the conversation to continue.' : '')
})
const openCodeRegionBlocked = computed(() => visibleError.value.startsWith('OpenCode Go requires Global regions for this model.'))
// The conversation already shows a failed run's reason as its failure card.
const errorShown = computed(() => {
  const run = detail.value?.run
  const reason = run?.status === 'failed' ? run.error : undefined
  if (error.value || detail.value?.error || !reason)
    return false
  // Quota bookkeeping hides some failures from the conversation: they show nothing.
  const hidden = quotaHiddenEvents(events.value)
  return events.value.some(event => event.runId === run?.id && !hidden.has(`${event.runId}:${event.id}`) && sameReason(failureReason(event) ?? '', reason) && !stopEcho(event))
})
watch(() => [visibleError.value, detail.value?.id, detail.value?.run?.finishedAt], () => {
  dismissedError.value = false
})
const busy = ref(false)
const submitting = ref(false)
const editing = ref<string | null>(null)
const agentId = ref(typeof route.query.agent === 'string' ? route.query.agent : MAIN_AGENT_ID)
const projectId = ref(typeof route.query.project === 'string' ? route.query.project : '')
const attachments = ref<ChatAttachment[]>([])
const previews = ref<Record<string, string>>({})
// Per-item upload state (A'): uploading during send, failed (retried by the next send), or rejected locally.
const attachmentStates = ref<Record<string, DraftState>>({})
const files = new Map<string, File>()
const uploaded = new Set<string>()
const dragging = ref(0)
const hasDraft = computed(() => !!draft.value.trim() || attachments.value.length > 0)
// Rejected files must be removed before sending; nothing is sent while a file uploads.
const attachmentsBlocked = computed(() => Object.values(attachmentStates.value).some(item => item.status === 'rejected' || item.status === 'uploading'))

function removeAttachment(id: string) {
  if (previews.value[id])
    URL.revokeObjectURL(previews.value[id])
  delete previews.value[id]
  delete attachmentStates.value[id]
  files.delete(id)
  uploaded.delete(id)
  attachments.value = attachments.value.filter(attachment => attachment.id !== id)
}

function clearAttachments() {
  for (const attachment of attachments.value)
    removeAttachment(attachment.id)
}

function addFiles(selected: File[]) {
  if (busy.value)
    return
  const selection = validateSelection(attachments.value.map(attachment => ({ size: attachment.size, rejected: attachmentStates.value[attachment.id]?.status === 'rejected' })), selected)
  if (selection.error) {
    error.value = selection.error
    return
  }

  error.value = ''
  for (const [index, file] of selected.entries()) {
    const id = crypto.randomUUID()
    const attachment: ChatAttachment = {
      id,
      name: file.name || 'pasted-image.png',
      size: file.size,
      kind: ['image/png', 'image/jpeg', 'image/webp', 'image/gif'].includes(file.type) ? 'image' : 'file',
      mediaType: file.type,
      chatId: '',
      digest: '',
      createdAt: 0,
    }
    const rejected = selection.rejected[index]
    // A rejected file is shown with its reason but never uploaded.
    if (rejected)
      attachmentStates.value[id] = { status: 'rejected', error: rejected }
    else
      files.set(id, file)
    if (isImage(attachment))
      previews.value[id] = URL.createObjectURL(file)
    attachments.value.push(attachment)
  }
}

// C': an attachment already sent in this chat goes back into the draft as the same record; it has
// no local file, so sending does not upload it again.
function reuseAttachment(attachment: ChatAttachment) {
  if (busy.value || !detail.value || attachment.chatId !== detail.value.id)
    return
  if (!attachments.value.some(item => item.id === attachment.id)) {
    if (attachments.value.length >= MAX_ATTACHMENTS) {
      error.value = `Attach up to ${MAX_ATTACHMENTS} files per message.`
      return
    }

    const total = attachments.value.reduce((sum, item) => sum + (attachmentStates.value[item.id]?.status === 'rejected' ? 0 : item.size), 0)
    if (total + attachment.size > MAX_MESSAGE_SIZE) {
      error.value = 'Attachments must total at most 40 MB per message.'
      return
    }

    error.value = ''
    attachments.value.push({ ...attachment })
  }

  textarea.value?.focus()
}

provide(reuseAttachmentKey, { chatId: () => detail.value?.id, reuse: reuseAttachment })

function dropFiles(event: DragEvent) {
  dragging.value = 0
  addFiles(Array.from(event.dataTransfer?.files ?? []))
}

function pasteFiles(event: ClipboardEvent) {
  const pasted = Array.from(event.clipboardData?.files ?? [])
  if (!pasted.length)
    return
  event.preventDefault()
  addFiles(pasted)
}

const active = computed(() => !!detail.value?.run && ['queued', 'running'].includes(detail.value.run.status))
const suspended = computed(() => isExclusivelySuspended(detail.value?.run))
// The plan of the current request stays in sight beside the composer while the agent follows it.
const currentPlan = computed(() => runActive.value && !suspended.value ? activity.value?.plan : undefined)
const recovery = useRetryState(() => detail.value?.run)
const scheduled = computed(() => scheduledWakeup(detail.value))
// The scheduled wait shows as a card in the conversation; these notices explain a run that cannot start.
const waitNotice = computed(() => recovery.value ? null : chatWaitNotice(detail.value?.run))
const waitNow = ref(Date.now())
let waitClock: ReturnType<typeof setInterval> | undefined
watch(() => !!scheduled.value, (waiting) => {
  clearInterval(waitClock)
  waitNow.value = Date.now()
  if (waiting)
    waitClock = setInterval(() => waitNow.value = Date.now(), 1000)
}, { immediate: true })
onBeforeUnmount(() => clearInterval(waitClock))
const sleepStatus = computed(() => {
  const wakeup = scheduled.value
  if (!wakeup)
    return null
  return waitStatus(wakeup, waitNow.value)
})
// One object per conversation state: a new one on each render would re-render the whole feed.
const liveWait = computed<LiveWait | undefined>(() => detail.value
  ? {
      wakeup: liveWakeup(detail.value),
      paused: detail.value.paused,
      settled: detail.value.run?.status === 'succeeded',
      busy: busy.value,
    }
  : undefined)
const backgroundTasks = computed(() => detail.value?.run?.status === 'running' ? backgroundWait(events.value)?.tasks.length ?? 0 : 0)
const chatStatus = computed(() => {
  if (quota.value && !detail.value?.paused)
    return 'Waiting for quota'
  const status = detail.value?.run?.status
  if (status === 'failed')
    return 'Failed'
  if (status === 'interrupted')
    return 'Interrupted'
  if (detail.value?.paused)
    return 'Paused'

  if (suspended.value)
    return exclusiveSuspensionLabel
  // "Waiting" names the Fil section of scheduled waits; a run waiting for a runner is queued.
  if (status === 'queued')
    return 'Queued'
  if (status === 'running') {
    if (backgroundTasks.value)
      return backgroundTasks.value === 1 ? 'Background task' : 'Background tasks'
    return 'Working'
  }

  if (sleepStatus.value)
    return sleepStatus.value
  return 'Ready'
})
const outgoing = ref<ChatMessage | null>(null)
const delivery = computed(() => chatDelivery(detail.value, events.value, outgoing.value))
const pending = computed(() => delivery.value.queued)
const responding = computed(() => active.value || delivery.value.sending.length > 0)
watch(events, (items) => {
  if (outgoing.value && items.some(event => event.type === 'chat.user' && event.payload?.messageId === outgoing.value?.id))
    outgoing.value = null
})
const selectedAgent = computed(() => state.agents.find(agent => agent.id === (detail.value?.agentId || agentId.value)))
const agents = computed(() => state.agents.map(agent => ({
  value: agent.id,
  label: agent.name,
  icon: Bot,
  description: agent.id === MAIN_AGENT_ID ? 'Full access' : agent.description,
})))
const currentProvider = computed(() => detail.value?.run?.snapshot.agent.provider || selectedAgent.value?.provider || 'codex')
const chosenProvider = computed({
  get: () => provider.value || currentProvider.value,
  set: (value: Provider) => {
    provider.value = value
    model.value = ''
    reasoning.value = ''
    speed.value = ''
  },
})
const switchingProvider = computed(() => !!detail.value?.run && chosenProvider.value !== currentProvider.value)
const planAvailable = computed(() => providers[chosenProvider.value].planTurns)
// Availability can block submission, but must not erase the requested boundary.
const planRequested = computed(() => planning.value)
const planBlocked = computed(() => planRequested.value && !planAvailable.value)
const planUnavailableMessage = 'Plan requires Codex or Claude Code. Choose a compatible agent, or turn Plan off to send an ordinary request.'
const canSend = computed(() => !planBlocked.value && !inactive.value && (!route.params.id || (detail.value?.id === route.params.id || detail.value?.slug === route.params.id)) && !live.error.value && (!!draft.value.trim() || attachments.value.length > 0) && !attachmentsBlocked.value)
// A plan request cannot join a turn that can already change files.
const steerBlocked = computed(() => planRequested.value && !detail.value?.run?.chatExecution?.plan)
const conversationAssistant = computed(() => detail.value?.run?.snapshot.agent)
const inheritConversation = computed(() => chosenProvider.value === conversationAssistant.value?.provider)
const inheritedAssistant = computed(() => {
  if (inheritConversation.value)
    return conversationAssistant.value
  return chosenProvider.value === selectedAgent.value?.provider ? selectedAgent.value : undefined
})
const skills = computed(() => chatSkills(state.skills, selectedAgent.value, detail.value ? detail.value.projectId : projectId.value))
const skillNames = computed(() => skills.value.map(skill => skill.name))
const projects = computed(() => [{
  value: '',
  label: 'No project',
  description: 'Use the agent’s available workspaces',
  icon: FolderGit2,
}, ...state.projects.filter(project => selectedAgent.value?.access.projects === null || selectedAgent.value?.access.projects.includes(project.id)).map(project => ({ value: project.id, label: project.name, icon: FolderGit2 }))])
const draftScope = state.installationId ? `${state.installationId}:` : ''
let createdChat: Chat | undefined
let draftKey = ''
let restoringDraft = false

function saveDraft(key = draftKey, text = draft.value, plan = planning.value) {
  if (!key)
    return
  sessionStorage.setItem(key, text)
  sessionStorage.setItem(`${key}:plan`, String(plan))
}

function loadDraft(keepPlanning = false) {
  restoringDraft = true
  try {
    draftKey = `cairn-chat-draft:${draftScope}${selectedChatId.value || `new:${agentId.value}:${projectId.value}`}`
    draft.value = sessionStorage.getItem(draftKey) ?? (typeof route.query.draft === 'string' ? route.query.draft : '')
    const savedPlan = sessionStorage.getItem(`${draftKey}:plan`)
    if (savedPlan !== null || !keepPlanning)
      planning.value = savedPlan === 'true'
  }
  finally {
    restoringDraft = false
  }
}

loadDraft()
watch(selectedChatId, (id, previous) => {
  // Text or an explicit Plan choice can arrive before a slug resolves to its UUID.
  if (id && previous === chatId.value && id !== previous) {
    const previousKey = `cairn-chat-draft:${draftScope}${previous}`
    const destinationKey = `cairn-chat-draft:${draftScope}${id}`
    const text = sessionStorage.getItem(previousKey) ?? draft.value
    const plan = sessionStorage.getItem(`${previousKey}:plan`)
    if (text)
      saveDraft(destinationKey, text, plan === 'true')
    else if (plan !== null)
      sessionStorage.setItem(`${destinationKey}:plan`, plan)
  }

  loadDraft(!!createdChat && (id === createdChat.id || chatId.value === createdChat.slug))
})
watch([draft, planning], () => {
  // Save user changes before a navigation can change the destination key.
  if (!restoringDraft)
    saveDraft()
}, { flush: 'sync' })
watch(agentId, () => {
  if (!projects.value.some(project => project.value === projectId.value))
    projectId.value = ''
})
watch(live.snapshot, (value) => {
  detail.value = value?.chat ?? null
  if (value?.chats)
    publishChats(value.chats, { saved: live.restored.value })
}, { immediate: true })
// The open conversation's finished turns are read while the page is visible.
useReadWhileOpen(() => {
  const chat = detail.value
  const run = chat?.run
  if (!chat || !live.synced.value || catchingUp.value || chat.id !== selectedChatId.value || run?.status !== 'succeeded' || !run.finishedAt)
    return null
  return { key: `chat:${chat.id}`, at: run.finishedAt }
})
// A stream error leaves on its own once the stream recovers; the connection pill covers the outage.
watch(live.error, (value, previous) => {
  if (value)
    error.value = value
  else if (error.value === previous)
    error.value = ''
})
onBeforeUnmount(clearAttachments)
let submission: {
  key: string
  message: {
    id: string
    text: string
    mode: 'queue' | 'steer'
    provider: Provider
    model: string
    reasoning: string
    plan: boolean
    speed: string
    attachmentIds: string[]
  }
} | undefined

function newConversation() {
  if (chatId.value)
    saveDraft()
  else
    saveDraft(draftKey, '', false)
  // The composer resets belong to the destination, not the previous draft.
  draftKey = ''
  outgoing.value = null
  history.value = false
  detailsOpen.value = false
  draft.value = ''
  clearAttachments()
  provider.value = ''
  model.value = ''
  reasoning.value = ''
  planning.value = false
  speed.value = ''
  agentId.value = MAIN_AGENT_ID
  projectId.value = ''
  createdChat = undefined
  submission = undefined
  editing.value = null
  error.value = ''
  void router.push('/chats').finally(() => loadDraft())
}

async function send(mode: 'queue' | 'steer' = 'queue') {
  const originalDraft = draft.value
  const text = originalDraft.trim()
  if (!canSend.value || busy.value)
    return
  submitting.value = true
  const origin = chatId.value
  const direct = !editing.value && ((mode === 'steer' && !detail.value?.paused) || (!responding.value && !detail.value?.paused && !pending.value.length))
  busy.value = true
  error.value = ''
  try {
    const chat = detail.value ?? createdChat ?? await createChat(api, { agentId: agentId.value, projectId: projectId.value || null })
    createdChat = chat
    // Items without a local file (reused or already sent records) are never uploaded again.
    const pendingUploads = attachments.value.filter(attachment => files.has(attachment.id) && !uploaded.has(attachment.id))
    for (const attachment of pendingUploads)
      attachmentStates.value[attachment.id] = { status: 'uploading' }
    const results = await Promise.allSettled(pendingUploads.map(async (attachment) => {
      try {
        const saved = await apiUpload<ChatAttachment>(`/chats/${chat.id}/attachments/${attachment.id}?name=${encodeURIComponent(attachment.name)}`, files.get(attachment.id)!, {
          headers: { 'Content-Type': 'application/octet-stream' },
          onProgress: (progress) => {
            const current = attachmentStates.value[attachment.id]
            if (current?.status === 'uploading')
              current.progress = progress
          },
        })
        Object.assign(attachment, saved)
        uploaded.add(attachment.id)
        delete attachmentStates.value[attachment.id]
      }
      catch (e) {
        // The item stays in the draft; the next send retries it.
        if (attachments.value.some(item => item.id === attachment.id))
          attachmentStates.value[attachment.id] = { status: 'failed', error: (e as Error).message || 'Upload failed.' }
        throw e
      }
    }))
    // Failed items show their error on the tile; the message waits for the next send.
    if (results.some(result => result.status === 'rejected'))
      return

    const attachmentIds = attachments.value.map(attachment => attachment.id)
    // A chat can use a different model from its saved agent. Keep the displayed
    // conversation selection explicit on the wire, including after a reload.
    const selectedModel = model.value || inheritedAssistant.value?.model || ''
    const selectedReasoning = reasoning.value || (!model.value ? inheritedAssistant.value?.reasoning : '') || ''
    // The speed survives a model change, so it is inherited even with a new model.
    const selectedSpeed = speed.value || inheritedAssistant.value?.speed || ''
    const content = {
      text,
      mode,
      provider: chosenProvider.value,
      model: selectedModel,
      reasoning: selectedReasoning,
      plan: planRequested.value,
      speed: selectedSpeed,
      attachmentIds,
    }
    const submissionKey = JSON.stringify({ chatId: chat.id, editing: editing.value, ...content })
    // Retain the id after a network failure only for the same target and content.
    if (submission?.key !== submissionKey) {
      submission = {
        key: submissionKey,
        message: { id: crypto.randomUUID(), ...content },
      }
    }

    if (direct) {
      outgoing.value = {
        ...submission.message,
        chatId: chat.id,
        revision: '',
        status: 'queued',
        createdAt: Date.now(),
        attachments: [...attachments.value],
      }
    }

    if (editing.value)
      await editChatMessage(api, { chatId: chat.id, messageId: editing.value, message: submission.message })
    else
      await sendChatMessage(api, { chatId: chat.id, message: submission.message })
    // Another conversation was opened meanwhile; its composer is not this message's.
    if (chatId.value !== origin) {
      submission = undefined
      return
    }

    if (draft.value === originalDraft)
      draft.value = ''
    editing.value = null
    clearAttachments()
    submission = undefined
    if (!detail.value) {
      saveDraft(`cairn-chat-draft:${draftScope}${chat.id}`)
      await router.push(chatPath(chat))
    }

    textarea.value?.focus()
  }
  catch (e) {
    outgoing.value = null
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
    submitting.value = false
  }
}

const newSession = ref(false)
const compacting = ref(false)
const context = computed(() => contextSummary(detail.value?.contextUsage))

async function action(name: 'pause' | 'stop' | 'restore' | 'new-session' | 'compact', body?: { paused?: boolean, confirm?: boolean }) {
  if (!detail.value || busy.value)
    return false
  busy.value = true
  error.value = ''
  try {
    const chatId = detail.value.id
    if (name === 'pause')
      await setChatPaused(api, { chatId, paused: body?.paused ?? false })
    else if (name === 'stop')
      await stopChat(api, { chatId })
    else if (name === 'restore')
      await restoreChat(api, { chatId })
    else if (name === 'new-session')
      await restartChatSession(api, { chatId, confirm: body?.confirm ?? false })
    else
      await compactChat(api, { chatId })
    return true
  }
  catch (e) {
    error.value = (e as Error).message
    return false
  }
  finally { busy.value = false }
}

async function resumeQuota() {
  const chat = detail.value
  if (!chat || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    if (chat.run?.status === 'failed')
      await resumeRun(api, { runId: chat.run.id })
    await setChatPaused(api, { chatId: chat.id, paused: false })
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally { busy.value = false }
}

async function update(message: ChatMessage, mode?: 'steer') {
  error.value = ''
  try {
    const reference = { chatId: detail.value!.id, messageId: message.id }
    if (mode)
      await editChatMessage(api, { ...reference, message: { ...message, mode, attachmentIds: message.attachments.map(attachment => attachment.id) } })
    else
      await deleteChatMessage(api, reference)
  }
  catch (e) { error.value = (e as Error).message }
}

function edit(message: ChatMessage) {
  if (hasDraft.value && !editing.value) {
    error.value = 'Send or clear your draft before editing a queued message.'
    return
  }

  editing.value = message.id
  clearAttachments()
  attachments.value = [...(message.attachments ?? [])]
  draft.value = message.text
  provider.value = message.provider || currentProvider.value
  model.value = message.model
  reasoning.value = message.reasoning || ''
  planning.value = message.plan === true
  speed.value = message.speed || ''
  textarea.value?.focus()
}

// Leaving plan mode is explicit: this queues an ordinary turn in the same conversation,
// with the agent's usual permissions.
async function implementPlan() {
  if (hasDraft.value || editing.value || active.value || pending.value.length || delivery.value.sending.length)
    return
  planning.value = false
  draft.value = 'Implement the plan you proposed.'
  await send()
}

// The view stays mounted across conversations: reset what belongs to the previous one.
watch(chatId, (id) => {
  openedSubagent.value = null
  history.value = false
  detailsOpen.value = false
  connectionOpen.value = false
  newSession.value = false
  editing.value = null
  error.value = ''
  clearAttachments()
  provider.value = ''
  model.value = ''
  reasoning.value = ''
  const opening = !!createdChat && createdChat.slug === id
  speed.value = ''
  submission = undefined
  const destinationId = opening ? createdChat!.id : selectedChatId.value
  createdChat = undefined
  // A first message keeps its optimistic bubble while the new chat opens.
  if (outgoing.value?.chatId !== destinationId)
    outgoing.value = null
  if (!id) {
    agentId.value = typeof route.query.agent === 'string' ? route.query.agent : MAIN_AGENT_ID
    projectId.value = typeof route.query.project === 'string' ? route.query.project : ''
  }

  // Plan mode follows the first message into the conversation it created.
  loadDraft(opening)
})

function key(event: KeyboardEvent) {
  if (event.key !== 'Enter' || event.shiftKey || event.isComposing)
    return
  // Touch keyboards keep Enter for newlines. The visible send button is always available.
  if (window.matchMedia('(pointer: coarse)').matches && !event.ctrlKey && !event.metaKey)
    return
  event.preventDefault()
  void send(event.altKey && active.value && !switchingProvider.value ? 'steer' : 'queue')
}
</script>

<template>
  <Modal v-if="compacting" title="Compact the agent context?" @close="compacting = false">
    <div class="p-6">
      <p>The coding agent summarizes its earlier context to free space in its window. This uses the agent's model, like a short turn.</p>
      <p>Every message stays visible in this conversation. Messages you send meanwhile wait for the compaction to finish.</p>
      <UiButton :disabled="busy || !detail?.compaction?.available" @click="action('compact').then((succeeded) => { if (succeeded) compacting = false })">
        Compact context
      </UiButton>
    </div>
  </Modal>
  <Modal v-if="newSession" title="Start a fresh agent session?" @close="newSession = false">
    <div class="p-6">
      <p>The conversation history and working files will be preserved. The next message will use a new native agent session.</p>
      <UiButton :disabled="busy" @click="action('new-session', { confirm: true }).then((succeeded) => { if (succeeded) newSession = false })">
        Confirm new session
      </UiButton>
    </div>
  </Modal>
  <Modal
    v-if="detailsOpen"
    title="Chat details"
    sheet
    :return-focus="detailsButton"
    @close="detailsOpen = false"
  >
    <div class="px-5 pb-5">
      <p class="mt-1! mb-5! text-sm leading-relaxed wrap-anywhere">
        {{ detail?.title || 'New conversation' }}
      </p>
      <dl class="grid grid-cols-[72px_minmax(0,1fr)] gap-x-3 gap-y-3 border-y border-line py-4 text-xs">
        <dt class="text-muted">
          Agent
        </dt><dd class="m-0 wrap-anywhere">
          {{ detail?.agentName || selectedAgent?.name || 'Main agent' }}
        </dd>
        <dt class="text-muted">
          Project
        </dt><dd class="m-0 wrap-anywhere">
          {{ detail?.projectName || state.projects.find(project => project.id === projectId)?.name || 'No project' }}
        </dd>
        <template v-if="detail?.requester">
          <dt class="text-muted">
            From
          </dt><dd class="m-0 wrap-anywhere">
            <RouterLink :to="chatPath({ id: detail.requester.chatId })" class="font-semibold text-accent" @click="detailsOpen = false">
              Delegated by {{ detail.requester.title || 'New conversation' }}
            </RouterLink>
          </dd>
        </template>
        <dt class="text-muted">
          Status
        </dt><dd class="m-0 flex items-center gap-2">
          <span class="size-1.5 rounded-full bg-accent" />{{ chatStatus }}
        </dd>
        <template v-if="detail?.run">
          <dt class="text-muted">
            Context
          </dt><dd class="m-0" data-testid="context-usage">
            <span class="block">{{ context.label }}</span>
            <span v-for="line in context.details" :key="line" class="block text-muted">{{ line }}</span>
          </dd>
        </template>
      </dl>
      <button class="chat-detail-action" @click="detailAction(newConversation)">
        <Icon :name="Plus" :size="18" />New chat
      </button>
      <button
        v-if="detail?.run"
        class="chat-detail-action"
        :disabled="!fileCount"
        @click="detailAction(() => activity?.openFiles())"
      >
        <Icon :name="FileText" :size="18" />Files <span class="ml-auto text-muted">{{ fileCount }}</span>
      </button>
      <template v-if="detail?.compaction?.supported && !inactive">
        <button
          class="chat-detail-action"
          aria-haspopup="dialog"
          :disabled="!detail.compaction.available"
          :aria-describedby="detail.compaction.reason ? 'compaction-reason' : undefined"
          @click="detailAction(() => { compacting = true })"
        >
          <Icon :name="Layers" :size="18" />{{ detail.compaction.running ? 'Compacting context…' : 'Compact context' }}
        </button>
        <p v-if="detail.compaction.reason && !detail.compaction.running" id="compaction-reason" class="m-0! mb-1! px-3 text-2xs text-muted">
          {{ detail.compaction.reason }}
        </p>
      </template>
      <button class="chat-detail-action" @click="detailAction(() => { history = true })">
        <Icon :name="Search" :size="18" />Search conversations
      </button>
      <RouterLink to="/settings/account" class="chat-detail-action" @click="detailsOpen = false">
        <Icon :name="Bell" :size="18" />Question notifications
      </RouterLink>
      <button class="chat-detail-action" aria-haspopup="dialog" @click="detailAction(() => { connectionOpen = true })">
        <Icon :name="Plug" :size="18" />Connection
      </button>
      <template v-if="detail?.run">
        <button class="chat-detail-action" @click="detailAction(() => activity?.followLatest())">
          <Icon :name="ArrowDown" :size="18" />Follow latest output
        </button>
        <button class="chat-detail-action" @click="detailAction(() => activity?.enterFullscreen())">
          <Icon :name="Maximize2" :size="18" />Open activity fullscreen
        </button>
      </template>
      <button
        v-if="detail && !inactive"
        class="chat-detail-action text-danger!"
        aria-haspopup="dialog"
        @click="detailAction(() => { deletingConversation = detail! })"
      >
        <Icon :name="Trash2" :size="18" />Delete conversation
      </button>
      <div class="border-t border-line pt-1">
        <button class="chat-detail-action" @click="detailAction(() => workspaceActions?.search())">
          <Icon :name="Search" :size="18" />Search workspace
        </button>
        <button class="chat-detail-action" @click="detailAction(() => workspaceActions?.navigation())">
          <Icon :name="Menu" :size="18" />Workspace navigation
        </button>
        <div class="flex min-h-12 items-center justify-between text-xs">
          <span>Appearance</span><ThemeControl compact />
        </div>
      </div>
    </div>
  </Modal>
  <ConversationDeletion
    v-if="deletingConversation"
    :chat="deletingConversation"
    @close="deletingConversation = undefined"
    @deleted="() => { deletingConversation = undefined; newConversation() }"
  />
  <ConnectionSheet
    v-if="connectionOpen"
    :run="detail?.run"
    :agent="detail?.agentName || selectedAgent?.name || 'Main agent'"
    :project="detail ? detail.projectName : state.projects.find(project => project.id === projectId)?.name"
    :return-focus="detailsButton"
    @close="connectionOpen = false"
  />
  <div class="flex h-full min-h-0 min-w-0 flex-1">
    <div class="chat-page flex h-full min-h-0 min-w-0 flex-1 flex-col gap-3 px-6 pb-4 pt-3 phone:gap-0 phone:px-3 phone:pb-2 phone:pt-[env(safe-area-inset-top)]">
      <ChatSwitcher
        v-if="history"
        :chats="chats"
        :selected="selectedChatId"
        :anchor="switcherAnchor"
        @close="history = false"
        @create="newConversation"
        @deleted="id => { if (id === selectedChatId) newConversation() }"
      />
      <!-- One row, as on Android: who and what on the left, files and the ⋯ menu on the right. -->
      <header class="chat-header flex min-h-15 shrink-0 items-center gap-3 border-b border-line/70 pb-3 phone:h-15 phone:gap-2 phone:pb-0">
        <h1 class="sr-only hidden phone:block">
          {{ heading?.title || 'New conversation' }}
        </h1>
        <RouterLink to="/" class="hidden size-11 shrink-0 place-items-center rounded-full text-ink hover:bg-hover phone:grid" aria-label="Back to the Fil">
          <Icon :name="ChevronLeft" :size="22" />
        </RouterLink>
        <!-- On phones the trigger is the conversation itself: agent, title and where it runs. -->
        <button
          ref="historyButton"
          class="hidden min-w-0 flex-1 items-center justify-start px-0.5 py-1 phone:flex"
          aria-label="Conversations"
          :aria-description="heading?.provider ? `Latest harness: ${providers[heading.provider].label}` : undefined"
          aria-haspopup="dialog"
          :aria-expanded="history"
          @click="history = true"
        >
          <span class="flex min-w-0 items-center gap-2.5">
            <AgentAvatar
              :name="heading?.agentName || selectedAgent?.name || 'Main agent'"
              :identity="heading?.agentId || selectedAgent?.id"
              :size="34"
              :harness="heading?.provider"
            />
            <span class="min-w-0 text-left">
              <span class="flex min-w-0 items-center gap-1 text-[14px] font-semibold leading-tight text-ink">
                <span class="truncate">{{ heading?.title || 'New conversation' }}</span><Icon :name="ChevronDown" :size="14" class="shrink-0 text-muted" />
              </span>
              <span class="mt-0.5 flex min-w-0 items-center gap-1.5 text-[11.5px] font-normal leading-tight text-muted" :title="currentInstallation ? `${currentInstallation.status} · ${routeLabel(state.transportRoute)}` : undefined">
                <span class="size-1.5 shrink-0 rounded-full" :class="connectionTone(currentInstallation, state.transportRoute)" />
                <span class="truncate">{{ [heading?.agentName || selectedAgent?.name || 'Main agent', currentInstallation?.name].filter(Boolean).join(' · ') }}</span>
              </span>
            </span>
          </span>
        </button>
        <div class="flex min-w-0 flex-1 items-center gap-3 phone:hidden">
          <AgentAvatar
            v-if="heading"
            :name="heading.agentName"
            :identity="heading.agentId"
            :size="36"
            :harness="heading.provider"
            :working="detail?.run?.status === 'running' && !detail.paused && !suspended && !quota"
          />
          <div class="min-w-0 flex-1">
            <div class="flex min-w-0 items-center gap-1">
              <h1 class="truncate text-base! font-semibold! leading-snug! tracking-tight!">
                {{ heading?.title || 'New conversation' }}
              </h1>
              <button
                ref="tabletHistoryButton"
                :class="iconButton"
                class="-my-2 hidden! size-10! tablet:inline-flex!"
                aria-label="Conversations"
                aria-haspopup="dialog"
                :aria-expanded="history"
                @click="history = true"
              >
                <Icon :name="ChevronDown" :size="16" />
              </button>
            </div>
            <div v-if="heading" class="m-0! mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted">
              <span class="flex min-w-0 items-center gap-1.5"><Icon :name="Bot" :size="13" /><span class="truncate max-w-40">{{ heading.agentName }}</span></span>
              <template v-if="heading.projectName">
                <span aria-hidden="true" class="text-muted/40">/</span>
                <span class="flex min-w-0 items-center gap-1.5"><Icon :name="FolderGit2" :size="13" /><span class="truncate max-w-48">{{ heading.projectName }}</span></span>
              </template>
              <RouterLink
                v-if="heading.requester"
                :to="chatPath({ id: heading.requester.chatId })"
                class="flex min-w-0 items-center gap-1.5 font-semibold text-accent hover:underline"
                data-testid="chat-requester"
              >
                <Icon :name="CornerDownRight" :size="13" /><span class="truncate max-w-56">Delegated by {{ heading.requester.title || 'New conversation' }}</span>
              </RouterLink>
              <span v-if="detail" class="flex items-center gap-1.5 font-semibold" :class="chatStatus === 'Working' ? 'text-accent' : chatStatus === 'Failed' || chatStatus === 'Interrupted' ? 'text-coral' : ''"><span v-if="chatStatus !== 'Working'" class="size-1 shrink-0 rounded-full" :class="active ? 'bg-accent' : 'bg-muted'" />{{ chatStatus }}<span v-if="chatStatus === 'Working'" class="working-wave"><i /><i /><i /></span></span>
              <span
                v-if="context.percent !== null || detail?.contextUsage"
                class="tabular-nums"
                :class="(context.percent ?? 0) >= 85 ? 'text-coral' : ''"
                :title="[context.label, ...context.details].join('\n')"
                data-testid="context-indicator"
              >Context {{ context.short }}</span>
              <NodeExecution v-if="detail?.run && state.installationRole === 'owner'" :key="detail.run.id" :run="detail.run" />
            </div>
          </div>
        </div>
        <div class="flex shrink-0 items-center gap-1">
          <button
            v-if="detail?.run && fileCount"
            class="mr-1 flex min-h-9 items-center gap-2 rounded-full bg-accent/12 px-3.5 text-xs font-semibold text-accent hover:bg-accent/20 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent phone:hidden"
            :aria-label="`Files: ${fileCount}`"
            @click="activity?.openFiles()"
          >
            <Icon :name="Layers" :size="16" />Files<span aria-hidden="true" class="grid h-4.5 min-w-4.5 place-items-center rounded-full bg-accent px-1 text-[10px] font-bold text-surface">{{ fileCount }}</span>
          </button>
          <button
            v-if="detail?.run"
            :class="iconButton"
            class="relative hidden! phone:inline-flex!"
            :aria-label="`Files: ${fileCount}`"
            :disabled="!fileCount"
            @click="activity?.openFiles()"
          >
            <Icon :name="Layers" :size="19" />
            <span v-if="fileCount" aria-hidden="true" class="absolute right-1 top-1 grid h-4 min-w-4 place-items-center rounded-full bg-accent px-1 text-[10px] font-bold text-surface">{{ fileCount }}</span>
          </button>
          <button
            v-if="detail?.run"
            :class="iconButton"
            class="phone:hidden!"
            aria-label="Open activity fullscreen"
            @click="activity?.enterFullscreen()"
          >
            <Icon :name="Maximize2" :size="18" />
          </button>
          <button
            ref="detailsButton"
            :class="iconButton"
            aria-label="Chat details"
            aria-haspopup="dialog"
            :aria-expanded="detailsOpen"
            @click="detailsOpen = true"
          >
            <Icon :name="MoreHorizontal" :size="20" />
          </button>
        </div>
      </header>
      <div class="flex min-h-0 flex-1 flex-col">
        <section
          class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
          :aria-busy="!!route.params.id && !detail"
          aria-label="Chat workspace"
        >
          <!-- A floating pill, as on Android: connection status never moves the conversation. -->
          <p v-if="connectionNotice" role="status" class="pointer-events-none absolute top-3 left-1/2 z-10 m-0! -translate-x-1/2 rounded-full border border-line bg-surface/95 px-3 py-1.5 text-xs whitespace-nowrap text-muted shadow-[0_4px_16px_#0000000f] backdrop-blur-sm">
            {{ connectionNotice }}
          </p>
          <!-- As on Android, a conversation opens on a centred loader. It appears only when
               opening takes a moment, so a quick open goes straight to the conversation. -->
          <div v-if="route.params.id && !detail" role="status" class="opening-loader flex flex-1 flex-col items-center justify-center gap-3 text-sm text-muted">
            <Icon :name="LoaderCircle" :size="22" class="animate-spin" />Loading conversation…
          </div>
          <div v-else-if="inactive" class="m-auto w-full max-w-xl px-6 py-12 text-center">
            <h2 class="mb-3 text-xl">
              {{ recoverable ? 'This conversation is in the trash.' : 'Permanently deleting conversation…' }}
            </h2>
            <p v-if="recoverable && detail?.purgeAt" class="mb-5 text-sm text-muted">
              Permanently deleted on {{ new Date(detail.purgeAt).toLocaleDateString() }}.
            </p>
            <p v-else class="mb-5 text-sm text-muted">
              This conversation cannot be restored. Its messages and working files are being removed.
            </p>
            <UiAlert v-if="error || detail?.lifecycleError" class="mb-4">
              {{ error || detail?.lifecycleError }}
            </UiAlert>
            <UiButton
              v-if="recoverable"
              :disabled="busy"
              variant="primary"
              @click="action('restore')"
            >
              Restore conversation
            </UiButton>
          </div>
          <div v-else-if="!detail?.run && !delivery.sending.length" class="flex min-h-0 flex-1 flex-col items-center justify-center overflow-auto px-6 pb-[8vh] pt-8 text-center phone:px-2 phone:py-5">
            <AgentAvatar
              v-if="selectedAgent"
              :name="selectedAgent.name"
              :identity="selectedAgent.id"
              :size="56"
              class="mb-4"
            />
            <h2 class="mb-7 text-[30px] font-semibold tracking-tight phone:text-2xl">
              What are we building?
            </h2>
            <div v-if="!detail" class="flex flex-wrap justify-center gap-2">
              <button
                v-for="idea in ['Explore this project', 'Review recent changes', 'Help me build…']"
                :key="idea"
                class="rounded-full bg-surface px-4 py-2.5 text-xs text-muted hover:bg-hover hover:text-ink"
                @click="draft = idea; textarea?.focus()"
              >
                {{ idea }}
              </button>
            </div>
          </div>
          <ActivityFeed
            v-else
            ref="activity"
            :key="streamPath"
            :cache-key="streamPath"
            :position="live.position.value"
            :deliverables="deliverables"
            :outcome="detail?.run?.status === 'succeeded' ? detail.run.outcome : null"
            :sending="delivery.sending"
            :events="events"
            :quota="quota"
            :chat-id="detail?.id"
            :coding-agent-switch="detail?.codingAgentSwitch"
            :can-manage-accounts="state.installationRole === 'owner'"
            :busy="busy"
            :wakeup="liveWait"
            :active="detail?.run?.status === 'running' && !quota"
            :suspended="suspended"
            :agent-id="detail?.agentId || selectedAgent?.id"
            :agent="detail?.agentName || selectedAgent?.name || 'Main agent'"
            :task="detail?.title || 'New conversation'"
            :more="live.hasOlder.value"
            :loading-older="live.loadingOlder.value"
            :older-error="live.olderError.value"
            :loading="catchingUp"
            :trimmed="0"
            :skills="skillNames"
            :subagent-bar="subagentBar"
            compact-toolbar
            chat
            :can-implement="!hasDraft && !editing && !busy && !inactive && !active && !pending.length && !delivery.sending.length"
            @resume-quota="resumeQuota"
            @coding-agent-switched="chosenProvider = $event"
            @load="live.loadOlder"
            @position="live.savePosition"
            @implement="implementPlan"
            @pause-wait="busy || action('pause', { paused: true })"
          />
          <div v-if="!inactive" class="mx-auto w-full max-w-205 shrink-0 px-5 pb-1 pt-3 phone:px-0 phone:pt-2">
            <UiNotice
              v-if="detail?.environmentState"
              role="status"
              class="mb-3"
              tone="info"
              title="Working environment expired"
            >
              <p>
                {{ detail.environmentState === 'cleaning' ? 'Removing the working environment after inactivity. Please wait before sending another message.' : 'The working environment was removed after inactivity. Your next message starts a fresh environment using the conversation history.' }}
                History, uploaded attachments, published files and shared links are retained. Unpushed changes and other working files are no longer available.
              </p>
              <p v-if="detail.environmentError">
                Cleanup will retry: {{ detail.environmentError }}
              </p>
            </UiNotice>
            <ChatQuestions
              v-if="detail"
              :questions="detail.questions || []"
              :active="active"
              :suspended="suspended"
              :highlighted="typeof route.query.question === 'string' ? route.query.question : undefined"
            />
            <section
              v-if="openCodeRegionBlocked && !dismissedError"
              role="alert"
              aria-label="OpenCode Privacy settings"
              class="mb-3 rounded-2xl border border-warning/25 bg-surface p-4 phone:p-3.5"
            >
              <div class="flex items-start gap-3">
                <span class="grid size-8 shrink-0 place-items-center rounded-xl bg-warning/10 text-warning"><Icon :name="Shield" :size="17" /></span>
                <div class="min-w-0 flex-1">
                  <h3 class="text-sm font-semibold text-ink">
                    This model needs a Privacy change
                  </h3>
                  <p class="mt-1 text-xs leading-relaxed text-muted">
                    OpenCode Go requires <strong class="text-ink">Global</strong> regions for this model. Review your workspace’s <strong class="text-ink">Privacy</strong> settings, or choose another model to keep your current settings.
                  </p>
                </div>
                <button
                  type="button"
                  :class="iconButton"
                  class="shrink-0"
                  aria-label="Dismiss error"
                  @click="dismissedError = true"
                >
                  <Icon :name="X" :size="14" />
                </button>
              </div>
              <div class="mt-3 flex flex-wrap items-center gap-2">
                <a
                  href="https://opencode.ai/console/"
                  target="_blank"
                  rel="noopener noreferrer"
                  class="inline-flex min-h-9 items-center gap-1.5 rounded-lg bg-accent px-3 text-xs font-semibold text-surface"
                >OpenCode settings<Icon :name="ArrowUpRight" :size="14" /></a>
                <UiButton size="small" :disabled="busy" @click="action('pause', { paused: false })">
                  Retry response
                </UiButton>
              </div>
              <p class="mt-2.5 text-2xs text-muted">
                To keep your current Privacy settings, <RouterLink :to="{ path: '/chats', query: { agent: detail?.agentId, project: detail?.projectId || undefined } }" class="font-medium text-accent underline">
                  choose another model in a new conversation
                </RouterLink>. This conversation and its files stay saved.
              </p>
            </section>
            <UiNotice
              v-else-if="visibleError && !dismissedError && !errorShown"
              role="alert"
              tone="danger"
              class="error mb-3"
            >
              {{ visibleError }}
              <template #action>
                <button
                  :class="iconButton"
                  class="shrink-0"
                  aria-label="Dismiss error"
                  @click="dismissedError = true"
                >
                  <Icon :name="X" :size="14" />
                </button>
              </template>
            </UiNotice>
            <RecoveryNotice
              v-if="recovery && !quota"
              :recovery="recovery"
              :busy="busy"
              @cancel="action('stop')"
            />
            <UiNotice
              v-else-if="waitNotice && !quota"
              role="status"
              class="mb-3"
              :tone="waitNotice.account ? 'warning' : 'info'"
              :icon="waitNotice.account ? KeyRound : Hourglass"
              :title="waitNotice.account ? `${waitNotice.account} account needed` : undefined"
            >
              <p>
                {{ waitNotice.message }}
              </p>
              <RouterLink v-if="waitNotice.account && state.installationRole === 'owner'" to="/connections" class="mt-1 inline-flex font-semibold text-accent underline">
                Open Connections
              </RouterLink>
            </UiNotice>
            <div v-if="detail?.canRequestNewSession" class="mb-3 rounded-lg border border-line p-3 text-sm">
              <p>If the restored native session is incompatible, start a fresh session using the preserved history and files.</p>
              <UiButton :disabled="busy" @click="newSession = true">
                Start a fresh agent session
              </UiButton>
            </div>
            <p v-if="detail?.sessionRestartRequested" class="mb-3 text-sm text-muted">
              Ready for a fresh session. Write a new message, then resume the queue.
            </p>
            <details v-if="detail?.messages.some(message => message.status === 'cancelled')" class="mb-3 rounded-lg border border-line p-3 text-xs">
              <summary class="cursor-pointer text-muted">
                Cancelled messages
              </summary>
              <div v-for="message in detail.messages.filter(message => message.status === 'cancelled')" :key="message.id" class="mt-3">
                <p class="whitespace-pre-wrap wrap-anywhere">
                  {{ message.text }}
                </p>
                <MessageAttachments v-if="message.attachments?.length" :attachments="message.attachments" class="mt-2 items-start!" />
                <button class="mt-2 text-accent" @click="draft = message.text; textarea?.focus()">
                  Copy to message
                </button>
              </div>
            </details>
            <PlanBar v-if="currentPlan" :plan="currentPlan" />
            <SubagentBar
              v-if="subagentBar"
              :agents="currentSubagents"
              :active="runActive"
              :selected="openedSubagent"
              @open="openSubagent"
            />
            <form
              class="chat-composer relative rounded-[26px] border border-line bg-surface p-1 focus-within:border-accent/50"
              @submit.prevent="send()"
              @dragenter.prevent="dragging++"
              @dragover.prevent
              @dragleave.prevent="dragging = Math.max(0, dragging - 1)"
              @drop.prevent="dropFiles"
              @paste="pasteFiles"
            >
              <!-- Queued follow-ups open the composer itself, sharing its border and corners, as on Android. -->
              <template v-if="pending.length || detail?.paused">
                <QueueStrip
                  :pending="pending"
                  :models="codexModels"
                  :paused="!!detail?.paused"
                  :working="active"
                  :busy="busy"
                  @steer="update($event, 'steer')"
                  @edit="edit"
                  @remove="update"
                  @toggle-pause="action('pause', { paused: !detail?.paused })"
                />
                <hr class="mx-3 border-line">
              </template>
              <div class="px-3 pt-2 pb-2">
                <div v-if="dragging" class="pointer-events-none absolute inset-0 z-10 flex items-center justify-center gap-2 rounded-xl border-2 border-dashed border-accent bg-surface/95 text-sm font-semibold text-accent">
                  <Icon :name="Paperclip" :size="20" />Drop files here
                </div>
                <!-- A new conversation chooses its agent and project; one still opening does not. -->
                <div v-if="!route.params.id" class="mb-3 flex max-w-100 items-center gap-1 phone:mb-2">
                  <VirtualSelect
                    v-model="agentId"
                    :options="agents"
                    label="Chat agent"
                    hide-label
                    compact
                    variant="ghost"
                  />
                  <VirtualSelect
                    v-model="projectId"
                    :options="projects"
                    label="Chat project"
                    hide-label
                    compact
                    variant="ghost"
                  />
                </div>
                <div v-if="editing" class="mb-2 flex items-center justify-between text-[11px] text-accent">
                  Editing queued message<button
                    type="button"
                    :class="iconButton"
                    aria-label="Cancel edit"
                    @click="editing = null; draft = ''; clearAttachments()"
                  >
                    <Icon :name="X" :size="14" />
                  </button>
                </div>
                <ComposerAttachments
                  v-if="attachments.length"
                  :attachments="attachments"
                  :previews="previews"
                  :states="attachmentStates"
                  :disabled="busy"
                  class="mb-1.5"
                  @remove="removeAttachment"
                />
                <p
                  v-if="planBlocked"
                  role="status"
                  class="px-2 text-sm text-danger"
                  data-testid="plan-unavailable"
                >
                  {{ planUnavailableMessage }}
                </p>
                <SkillTextarea
                  ref="textarea"
                  v-model="draft"
                  :skills="skills"
                  aria-label="Message"
                  :placeholder="planRequested ? 'Describe what to plan… The agent proposes without changing anything' : liveWait?.wakeup ? (detail?.paused ? 'Add to the queue — this cancels the wait' : 'Reply now — this cancels the wait') : responding ? 'Add a follow-up…' : skills.length ? 'Message your agent… Type $ for skills' : 'Message your agent…'"
                  rows="2"
                  maxlength="50000"
                  @keydown="key"
                  @skills-open="refreshSkills().catch(() => {})"
                />
                <div class="flex items-center justify-between gap-2 pt-2">
                  <div class="flex min-w-0 flex-1 items-center gap-1">
                    <AttachMenu
                      :trigger-class="iconButton"
                      :disabled="busy || attachments.length >= MAX_ATTACHMENTS"
                      @files="addFiles"
                    />
                    <AssistantPicker
                      v-model:provider="chosenProvider"
                      v-model:model="model"
                      v-model:reasoning="reasoning"
                      v-model:speed="speed"
                      :inherit="!!inheritedAssistant"
                      :default-model="inheritedAssistant?.model || ''"
                      :default-reasoning="inheritedAssistant?.reasoning || ''"
                      :default-speed="inheritedAssistant?.speed || ''"
                      :default-source="inheritConversation ? 'conversation' : 'agent'"
                      :switching="switchingProvider"
                      :disabled="busy"
                    />
                    <button
                      type="button"
                      class="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-full px-3 text-sm font-medium disabled:opacity-50"
                      :class="planRequested ? 'bg-soft text-accent' : 'text-muted hover:bg-hover'"
                      :aria-pressed="planRequested"
                      aria-label="Plan first"
                      :title="planAvailable ? 'Plan first: the agent explores and proposes an approach without changing files, using the network from its shell or calling tools with external effects. Implement it afterwards in this conversation.' : planUnavailableMessage"
                      :disabled="busy || (!planAvailable && !planRequested)"
                      data-testid="plan-toggle"
                      @click="planning = !planning"
                    >
                      <Icon :name="NotebookPen" :size="15" />Plan
                    </button>
                  </div>
                  <!-- As on Android: steer while typing during a reply, stop when the field is empty, otherwise send. -->
                  <div class="flex shrink-0 items-center gap-1">
                    <button
                      v-if="active && !editing && hasDraft"
                      type="button"
                      class="round-action bg-soft text-accent"
                      aria-label="Steer now"
                      title="Send into the current turn (Alt + Enter)"
                      :disabled="busy || !canSend || switchingProvider || steerBlocked"
                      @click="send('steer')"
                    >
                      <Icon :name="Zap" :size="16" />
                    </button>
                    <button
                      v-if="(active || detail?.wakeup) && !editing && !hasDraft"
                      type="button"
                      class="round-action border border-line text-coral"
                      :aria-label="scheduled ? 'Cancel scheduled wait' : 'Stop response'"
                      :title="scheduled ? 'Cancel wakeup and pause queue' : 'Stop response and pause queue'"
                      :disabled="busy"
                      @click="action('stop')"
                    >
                      <span class="size-3 rounded-[3px] bg-current" />
                    </button>
                    <button
                      v-else
                      type="submit"
                      class="round-action"
                      :class="canSend && !busy ? 'bg-accent text-surface' : 'bg-ink/10 text-ink/40'"
                      :aria-label="submitting ? 'Sending…' : editing ? 'Save' : responding || detail?.paused ? 'Queue' : 'Send'"
                      :title="editing ? 'Save' : responding || detail?.paused ? 'Add to the queue' : 'Send'"
                      :disabled="busy || !canSend"
                    >
                      <Icon
                        v-if="submitting"
                        :name="LoaderCircle"
                        :size="18"
                        class="animate-spin"
                      />
                      <Icon v-else :name="editing ? Check : responding || detail?.paused ? Plus : ArrowUp" :size="18" />
                    </button>
                  </div>
                </div>
              </div>
            </form>
          </div>
        </section>
      </div>
    </div>
    <aside v-if="subagentTeam && wide" class="w-100 shrink-0 border-l border-line" aria-label="Subagent activity">
      <SubagentPanel
        :team="subagentTeam"
        :selected="openedSubagent!"
        :active="runActive"
        @select="openedSubagent = $event"
        @close="closeSubagent"
      />
    </aside>
  </div>
  <Modal
    v-if="subagentTeam && !wide"
    title="Subagent activity"
    drawer
    headless
    @close="closeSubagent"
  >
    <SubagentPanel
      :team="subagentTeam"
      :selected="openedSubagent!"
      :active="runActive"
      @select="openedSubagent = $event"
      @close="closeSubagent"
    />
  </Modal>
</template>

<style scoped>
.chat-detail-action {
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 48px;
  width: 100%;
  padding: 8px 0;
  text-align: left;
  font-size: 12px;
}
.chat-detail-action:hover { color: var(--color-accent); }
.chat-detail-action:disabled { opacity: .5; cursor: default; }
.round-action {
  display: grid;
  width: 40px;
  height: 40px;
  flex-shrink: 0;
  place-items: center;
  border-radius: 9999px;
  transition: background-color .15s, color .15s, opacity .15s;
}
.round-action:disabled { opacity: .5; cursor: default; }
</style>
