<script setup lang="ts">
import type {
  Chat,
  ChatAttachment,
  ChatDetail,
  ChatMessage,
} from '../../shared/chats'
import {
  computed,
  inject,
  nextTick,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { latestArtifacts } from '../../shared/artifacts'
import { MAIN_AGENT_ID } from '../../shared/constants'
import { api, state } from '../api'
import { chatDelivery, chatWaitNotice } from '../chat-delivery'
import { chatList as chats, publishChats } from '../chat-list'
import ActivityFeed from '../components/ActivityFeed.vue'
import AgentAvatar from '../components/AgentAvatar.vue'
import AssistantPicker from '../components/AssistantPicker.vue'
import ChatAttachments from '../components/ChatAttachments.vue'
import ChatQuestions from '../components/ChatQuestions.vue'
import ChatSwitcher from '../components/ChatSwitcher.vue'
import FilColumn from '../components/FilColumn.vue'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import NodeExecution from '../components/NodeExecution.vue'
import NotificationSettings from '../components/NotificationSettings.vue'
import QueueStrip from '../components/QueueStrip.vue'
import SkillTextarea from '../components/SkillTextarea.vue'
import ThemeControl from '../components/ThemeControl.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  ArrowDown,
  ArrowUp,
  Bell,
  Bot,
  Check,
  ChevronDown,
  ChevronLeft,
  FileText,
  FolderGit2,
  LoaderCircle,
  Maximize2,
  Menu,
  MessageCircle,
  MoreHorizontal,
  Paperclip,
  Plus,
  Search,
  X,
  Zap,
} from '../icons'
import { backgroundWait } from '../signal'
import { chatSkills } from '../skill-mentions'
import { iconButton } from '../ui'
import { useLiveRun } from '../use-live-run'
import { workspaceActionsKey } from '../workspace-actions'

const router = useRouter()
const route = useRoute()
const detail = ref<(ChatDetail & { error?: string }) | null>(null)
const inactive = computed(() => !!detail.value?.lifecycle && detail.value.lifecycle !== 'active')
const chatId = computed(() => typeof route.params.id === 'string' ? route.params.id : undefined)
const streamPath = computed(() => chatId.value ? `/chats/${chatId.value}/stream` : '/chats/stream')
const live = useLiveRun(() => streamPath.value, { hold: 1000 })
const { events, connectionNotice, catchingUp } = live
// The previous conversation stays visible until the selected one is ready.
const switching = computed(() => live.shown.value !== streamPath.value)
const deliverables = computed(() => live.snapshot.value?.artifacts ?? [])
const draft = ref('')
const model = ref('')
const reasoning = ref('')
const provider = ref<'' | 'codex' | 'claude'>('')
const notifications = ref(false)
const history = ref(false)
const detailsOpen = ref(false)
const detailsButton = ref<HTMLButtonElement>()
const activity = ref<InstanceType<typeof ActivityFeed>>()
const workspaceActions = inject(workspaceActionsKey)
const fileCount = computed(() => latestArtifacts(deliverables.value).length)

async function detailAction(action: () => void) {
  detailsOpen.value = false
  // Restore focus to the toolbar before opening another modal or navigation.
  await nextTick()
  action()
}

const historyButton = ref<HTMLButtonElement>()

const error = ref('')
const dismissedError = ref(false)
const visibleError = computed(() => error.value || detail.value?.error || (detail.value?.run?.status === 'failed' ? detail.value.run.error || 'The response stopped before finishing. Resume the conversation to continue.' : ''))
watch(() => [visibleError.value, detail.value?.id, detail.value?.run?.finishedAt], () => {
  dismissedError.value = false
})
const busy = ref(false)
const submitting = ref(false)
const editing = ref<string | null>(null)
const agentId = ref(typeof route.query.agent === 'string' ? route.query.agent : MAIN_AGENT_ID)
const projectId = ref(typeof route.query.project === 'string' ? route.query.project : '')
const textarea = ref<InstanceType<typeof SkillTextarea>>()
const fileInput = ref<HTMLInputElement>()
const attachments = ref<ChatAttachment[]>([])
const previews = ref<Record<string, string>>({})
const files = new Map<string, File>()
const uploaded = new Set<string>()
const dragging = ref(0)
const uploadProgress = ref('')
const hasDraft = computed(() => !!draft.value.trim() || attachments.value.length > 0)
const canSend = computed(() => !inactive.value && (!route.params.id || detail.value?.id === route.params.id) && !live.error.value && (!!draft.value.trim() || attachments.value.length > 0))

function removeAttachment(id: string) {
  if (previews.value[id])
    URL.revokeObjectURL(previews.value[id])
  delete previews.value[id]
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
  if (attachments.value.length + selected.length > 8) {
    error.value = 'Attach up to 8 files per message.'
    return
  }

  if (selected.some(file => file.size > 10 * 1024 * 1024)) {
    error.value = 'Files must be 10 MB or smaller.'
    return
  }

  if ([...attachments.value, ...selected].reduce((total, file) => total + file.size, 0) > 40 * 1024 * 1024) {
    error.value = 'Attachments must total at most 40 MB per message.'
    return
  }

  error.value = ''
  for (const file of selected) {
    const id = crypto.randomUUID()
    const image = ['image/png', 'image/jpeg', 'image/webp', 'image/gif'].includes(file.type)
    files.set(id, file)
    if (image)
      previews.value[id] = URL.createObjectURL(file)
    attachments.value.push({
      id,
      name: file.name || 'pasted-image.png',
      size: file.size,
      kind: image ? 'image' : 'file',
      mediaType: file.type,
      chatId: '',
    })
  }
}

function pickFiles(event: Event) {
  const input = event.target as HTMLInputElement
  addFiles(Array.from(input.files ?? []))
  input.value = ''
}

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
const waitNotice = computed(() => chatWaitNotice(detail.value?.run))
const backgroundTasks = computed(() => detail.value?.run?.status === 'running' ? backgroundWait(events.value)?.tasks.length ?? 0 : 0)
const chatStatus = computed(() => detail.value?.paused ? 'Paused' : active.value ? detail.value?.run?.status === 'queued' ? 'Waiting' : backgroundTasks.value ? backgroundTasks.value === 1 ? 'Background task' : 'Background tasks' : 'Working' : detail.value?.run?.status === 'failed' ? 'Failed' : detail.value?.run?.status === 'interrupted' ? 'Interrupted' : 'Ready')
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
  set: (value: 'codex' | 'claude') => {
    provider.value = value
    model.value = ''
    reasoning.value = ''
  },
})
const switchingProvider = computed(() => !!detail.value?.run && chosenProvider.value !== currentProvider.value)
const inheritAgentModel = computed(() => chosenProvider.value === (selectedAgent.value?.provider || 'codex'))
const skills = computed(() => chatSkills(state.skills, selectedAgent.value, detail.value ? detail.value.projectId : projectId.value))
const skillNames = computed(() => skills.value.map(skill => skill.name))
const projects = computed(() => [{
  value: '',
  label: 'No project',
  description: 'Use the agent’s available workspaces',
  icon: FolderGit2,
}, ...state.projects.filter(project => selectedAgent.value?.access.projects === null || selectedAgent.value?.access.projects.includes(project.id)).map(project => ({ value: project.id, label: project.name, icon: FolderGit2 }))])
let draftKey = ''

function loadDraft() {
  draftKey = `leo-chat-draft:${chatId.value || `new:${agentId.value}:${projectId.value}`}`
  draft.value = sessionStorage.getItem(draftKey) ?? (typeof route.query.draft === 'string' ? route.query.draft : '')
}

loadDraft()
watch(draft, value => sessionStorage.setItem(draftKey, value))
watch(agentId, () => {
  if (!projects.value.some(project => project.value === projectId.value))
    projectId.value = ''
})
watch(live.snapshot, (value) => {
  detail.value = value?.chat ?? null
  if (value?.chats)
    publishChats(value.chats)
})
watch(live.error, (value) => {
  if (value)
    error.value = value
})
onBeforeUnmount(clearAttachments)
let submission: {
  id: string
  text: string
  mode: 'queue' | 'steer'
  provider: 'codex' | 'claude'
  model: string
  reasoning: string
  attachmentIds: string[]
} | undefined
let createdChat: Chat | undefined

function newConversation() {
  outgoing.value = null
  history.value = false
  detailsOpen.value = false
  draft.value = ''
  clearAttachments()
  provider.value = ''
  model.value = ''
  reasoning.value = ''
  agentId.value = MAIN_AGENT_ID
  projectId.value = ''
  createdChat = undefined
  editing.value = null
  void router.push('/chats')
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
    const chat = detail.value ?? createdChat ?? await api<Chat>('/chats', { method: 'POST', body: JSON.stringify({ agentId: agentId.value, projectId: projectId.value || null }) })
    createdChat = chat
    for (const [index, attachment] of attachments.value.entries()) {
      const file = files.get(attachment.id)
      if (!file || uploaded.has(attachment.id))
        continue
      uploadProgress.value = `Uploading ${index + 1} of ${attachments.value.length}…`
      const saved = await api<ChatAttachment>(`/chats/${chat.id}/attachments/${attachment.id}?name=${encodeURIComponent(attachment.name)}`, { method: 'PUT', headers: { 'Content-Type': 'application/octet-stream' }, body: file })
      Object.assign(attachment, saved)
      uploaded.add(attachment.id)
    }

    uploadProgress.value = ''
    const attachmentIds = attachments.value.map(attachment => attachment.id)
    // Retain the id after a network failure so retry cannot duplicate a message.
    if (!submission || submission.text !== text || submission.mode !== mode || submission.provider !== chosenProvider.value || submission.model !== model.value || submission.reasoning !== reasoning.value || submission.attachmentIds.join() !== attachmentIds.join()) {
      submission = {
        id: crypto.randomUUID(),
        text,
        mode,
        provider: chosenProvider.value,
        model: model.value,
        reasoning: reasoning.value,
        attachmentIds,
      }
    }

    if (direct) {
      outgoing.value = {
        ...submission,
        chatId: chat.id,
        status: 'queued',
        createdAt: Date.now(),
        attachments: [...attachments.value],
      }
    }

    await api(`/chats/${chat.id}/messages${editing.value ? `/${editing.value}` : ''}`, { method: editing.value ? 'PUT' : 'POST', body: JSON.stringify(submission) })
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
      sessionStorage.setItem(`leo-chat-draft:${chat.id}`, draft.value)
      await router.push(`/chats/${chat.id}`)
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
    uploadProgress.value = ''
  }
}

const newSession = ref(false)

async function action(name: 'pause' | 'stop' | 'restore' | 'new-session', body?: object) {
  if (!detail.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    await api(`/chats/${detail.value.id}/${name}`, { method: 'POST', ...(body ? { body: JSON.stringify(body) } : {}) })
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}

async function update(message: ChatMessage, mode?: 'steer') {
  error.value = ''
  try {
    await api(`/chats/${detail.value!.id}/messages/${message.id}`, { method: mode ? 'PUT' : 'DELETE', ...(mode ? { body: JSON.stringify({ ...message, mode, attachmentIds: message.attachments?.map(attachment => attachment.id) ?? [] }) } : {}) })
  }
  catch (e) { error.value = (e as Error).message }
}

function edit(message: ChatMessage) {
  if (canSend.value && !editing.value) {
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
  textarea.value?.focus()
}

// The view stays mounted across conversations: reset what belongs to the previous one.
watch(chatId, (id) => {
  history.value = false
  detailsOpen.value = false
  newSession.value = false
  editing.value = null
  error.value = ''
  clearAttachments()
  provider.value = ''
  model.value = ''
  reasoning.value = ''
  submission = undefined
  createdChat = undefined
  // A first message keeps its optimistic bubble while the new chat opens.
  if (outgoing.value?.chatId !== id)
    outgoing.value = null
  if (!id) {
    agentId.value = typeof route.query.agent === 'string' ? route.query.agent : MAIN_AGENT_ID
    projectId.value = typeof route.query.project === 'string' ? route.query.project : ''
  }

  loadDraft()
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
  <Modal v-if="newSession" title="Start a fresh agent session?" @close="newSession = false">
    <div class="p-6">
      <p>The conversation history and working files will be preserved. The next message will use a new native agent session.</p>
      <UiButton :disabled="busy" @click="action('new-session', { confirm: true }).then(() => { if (!error) newSession = false })">
        Confirm new session
      </UiButton>
    </div>
  </Modal>
  <Modal v-if="notifications" title="Notifications" @close="notifications = false">
    <div class="p-6">
      <NotificationSettings />
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
        <dt class="text-muted">
          Status
        </dt><dd class="m-0 flex items-center gap-2">
          <span class="size-1.5 rounded-full bg-accent" />{{ chatStatus }}
        </dd>
      </dl>
      <button
        v-if="detail?.run"
        class="chat-detail-action"
        :disabled="!fileCount"
        @click="detailAction(() => activity?.openFiles())"
      >
        <Icon :name="FileText" :size="18" />Files <span class="ml-auto text-muted">{{ fileCount }}</span>
      </button>
      <button class="chat-detail-action" @click="detailAction(() => { history = true })">
        <Icon :name="Search" :size="18" />Search conversations
      </button>
      <button class="chat-detail-action" @click="detailAction(() => { notifications = true })">
        <Icon :name="Bell" :size="18" />Question notifications
      </button>
      <template v-if="detail?.run">
        <button class="chat-detail-action" @click="detailAction(() => activity?.followLatest())">
          <Icon :name="ArrowDown" :size="18" />Follow latest output
        </button>
        <button class="chat-detail-action" @click="detailAction(() => activity?.enterFullscreen())">
          <Icon :name="Maximize2" :size="18" />Open activity fullscreen
        </button>
      </template>
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
  <div class="flex h-full min-h-0">
    <aside class="w-88 shrink-0 border-r border-line bg-inset tablet:hidden" aria-label="Fil">
      <FilColumn :chats="chats" :selected="chatId" compact />
    </aside>
    <div class="chat-page flex h-full min-h-0 min-w-0 flex-1 flex-col gap-3 px-6 pb-4 pt-3 phone:gap-0 phone:px-3 phone:pb-2 phone:pt-[env(safe-area-inset-top)]">
      <ChatSwitcher
        v-if="history"
        :chats="chats"
        :selected="chatId"
        :anchor="historyButton"
        @close="history = false"
        @create="newConversation"
      />
      <header class="chat-header flex shrink-0 items-center justify-between gap-2 border-b border-line/70 pb-3 phone:h-15 phone:pb-0">
        <h1 v-if="detail" class="sr-only hidden phone:block">
          {{ detail.title }}
        </h1>
        <RouterLink to="/" class="hidden size-11 shrink-0 place-items-center rounded-full text-ink hover:bg-hover phone:grid" aria-label="Back to the Fil">
          <Icon :name="ChevronLeft" :size="22" />
        </RouterLink>
        <button
          ref="historyButton"
          class="flex min-h-10 items-center gap-2 rounded-lg border border-accent/25 bg-accent/10 px-3 py-2 text-xs font-semibold text-accent phone:min-w-0 phone:flex-1 phone:justify-start phone:border-0 phone:bg-transparent phone:px-1 phone:py-1"
          aria-label="Conversations"
          aria-haspopup="dialog"
          :aria-expanded="history"
          @click="history = true"
        >
          <span class="min-w-0">
            <span class="flex items-center gap-2"><Icon :name="MessageCircle" :size="18" />Conversations
              <span class="rounded bg-accent/10 px-1.5 py-0.5 text-[10px] phone:hidden">{{ chats.length }}</span>
              <Icon :name="ChevronDown" :size="14" />
            </span>
            <span class="mt-0.5 hidden truncate text-left text-[11px] font-normal text-muted phone:block">{{ detail?.title || 'New conversation' }}</span>
          </span>
        </button>
        <div class="flex items-center gap-1">
          <button
            :class="iconButton"
            class="phone:hidden!"
            aria-label="Question notifications"
            @click="notifications = true"
          >
            <Icon :name="Bell" :size="19" />
          </button>
          <RouterLink
            to="/chats"
            aria-label="New chat"
            class="flex min-h-10 shrink-0 items-center gap-2 whitespace-nowrap rounded-lg border border-line px-3 py-2 text-xs font-medium text-muted hover:bg-hover hover:text-ink phone:min-h-11 phone:min-w-11 phone:justify-center phone:border-transparent phone:px-2"
            @click.prevent="newConversation"
          >
            <Icon :name="Plus" :size="18" /><span class="phone:hidden">New chat</span>
          </RouterLink>
          <button
            ref="detailsButton"
            :class="iconButton"
            class="hidden! phone:inline-flex!"
            aria-label="Chat details"
            aria-haspopup="dialog"
            :aria-expanded="detailsOpen"
            @click="detailsOpen = true"
          >
            <Icon :name="MoreHorizontal" :size="20" />
          </button>
        </div>
      </header>
      <div v-if="detail" class="phone:hidden mx-auto flex w-full max-w-205 shrink-0 items-start gap-3.5 px-9 pb-1 pt-3 phone:px-4 phone:pt-1">
        <AgentAvatar
          :name="detail.agentName"
          :identity="detail.agentId"
          :size="40"
          :working="detail.run?.status === 'running' && !detail.paused"
          class="mt-0.5"
        />
        <div class="min-w-0 flex-1">
          <h1 class="line-clamp-2 text-xl! leading-snug! tracking-tight! phone:text-base! wrap-anywhere">
            {{ detail.title }}
          </h1>
          <div class="m-0! mt-1.5! flex flex-wrap items-center gap-x-2 gap-y-1 text-[11px] text-muted phone:mt-1! phone:text-[10px]">
            <span class="flex min-w-0 items-center gap-1.5"><Icon :name="Bot" :size="13" /><span class="truncate max-w-40">{{ detail.agentName }}</span></span>
            <template v-if="detail.projectName">
              <span aria-hidden="true" class="text-muted/40">/</span>
              <span class="flex min-w-0 items-center gap-1.5"><Icon :name="FolderGit2" :size="13" /><span class="truncate max-w-48 phone:max-w-32">{{ detail.projectName }}</span></span>
            </template>
            <span class="flex items-center gap-1.5 font-semibold" :class="chatStatus === 'Working' ? 'text-accent' : chatStatus === 'Failed' || chatStatus === 'Interrupted' ? 'text-coral' : ''"><span v-if="chatStatus !== 'Working'" class="size-1 shrink-0 rounded-full" :class="active ? 'bg-accent' : 'bg-muted'" />{{ chatStatus }}<span v-if="chatStatus === 'Working'" class="working-wave"><i /><i /><i /></span></span>
            <NodeExecution v-if="detail.run" :key="detail.run.id" :run="detail.run" />
          </div>
        </div>
      </div>
      <h1 v-else class="sr-only">
        Chats
      </h1>
      <div v-if="detail?.run" class="hidden phone:block">
        <NodeExecution :key="detail.run.id" :run="detail.run" class="mx-4 mb-2" />
      </div>
      <div class="flex min-h-0 flex-1 flex-col">
        <section
          class="relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden transition-opacity"
          :class="switching ? 'opacity-60 delay-150' : ''"
          :aria-busy="switching"
          aria-label="Chat workspace"
        >
          <!-- A floating pill, as on Android: connection status never moves the conversation. -->
          <p v-if="connectionNotice" role="status" class="pointer-events-none absolute top-3 left-1/2 z-10 m-0! -translate-x-1/2 rounded-full border border-line bg-surface/95 px-3 py-1.5 text-xs whitespace-nowrap text-muted shadow-[0_4px_16px_#0000000f] backdrop-blur-sm">
            {{ connectionNotice }}
          </p>
          <div v-if="route.params.id && !detail" role="status" class="flex flex-1 items-center justify-center text-sm text-muted">
            Loading conversation…
          </div>
          <div v-else-if="inactive" class="m-auto w-full max-w-xl px-6 py-12 text-center">
            <h2 class="mb-3 text-xl">
              {{ detail?.lifecycle === 'purging' ? 'Permanently deleting conversation…' : 'This conversation is in the trash.' }}
            </h2>
            <p v-if="detail?.purgeAt" class="mb-5 text-sm text-muted">
              Permanently deleted on {{ new Date(detail.purgeAt).toLocaleDateString() }}.
            </p>
            <p v-else class="mb-5 text-sm text-muted">
              Restore this conversation to access its history and working files.
            </p>
            <UiAlert v-if="error || detail?.lifecycleError" class="mb-4">
              {{ error || detail?.lifecycleError }}
            </UiAlert>
            <UiButton
              v-if="detail?.lifecycle === 'trash'"
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
            :key="live.shown.value"
            :cache-key="live.shown.value"
            :position="live.position.value"
            :deliverables="deliverables"
            :outcome="detail?.run?.status === 'succeeded' ? detail.run.outcome : null"
            :sending="delivery.sending"
            :events="events"
            :active="detail?.run?.status === 'running'"
            :agent-id="detail?.agentId || selectedAgent?.id"
            :agent="detail?.agentName || selectedAgent?.name || 'Main agent'"
            :task="detail?.title || 'New conversation'"
            :more="live.hasOlder.value"
            :loading-older="live.loadingOlder.value"
            :older-error="live.olderError.value"
            :loading="catchingUp"
            :trimmed="0"
            :skills="skillNames"
            chat
            @load="live.loadOlder"
            @position="live.savePosition"
          />
          <div v-if="!inactive" class="mx-auto w-full max-w-205 shrink-0 px-5 pb-1 pt-3 phone:px-0 phone:pt-2">
            <ChatQuestions
              v-if="detail"
              :questions="detail.questions || []"
              :active="active"
              :highlighted="typeof route.query.question === 'string' ? route.query.question : undefined"
            />
            <UiAlert v-if="visibleError && !dismissedError" class="mb-3 flex items-start gap-2">
              <span class="min-w-0 flex-1">{{ visibleError }}</span><button
                :class="iconButton"
                class="shrink-0"
                aria-label="Dismiss error"
                @click="dismissedError = true"
              >
                <Icon :name="X" :size="14" />
              </button>
            </UiAlert>
            <div v-if="waitNotice" class="mb-3 rounded-lg border border-line bg-soft px-4 py-3 text-sm" role="status">
              <p v-if="waitNotice.account" class="mb-1 font-semibold">
                {{ waitNotice.account }} account needed
              </p>
              <p>{{ waitNotice.message }}</p>
              <RouterLink v-if="waitNotice.account" to="/connections" class="mt-2 inline-flex font-semibold text-accent underline">
                Open Connections
              </RouterLink>
            </div>
            <div v-if="detail?.restoredAt && ['failed', 'interrupted'].includes(detail.run?.status ?? '') && !detail.sessionRestartRequested" class="mb-3 rounded-lg border border-line p-3 text-sm">
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
                <ChatAttachments v-if="message.attachments?.length" :attachments="message.attachments" />
                <button class="mt-2 text-accent" @click="draft = message.text; textarea?.focus()">
                  Copy to message
                </button>
              </div>
            </details>
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
                <div v-if="!detail" class="mb-3 flex max-w-100 items-center gap-1 phone:mb-2">
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
                <ChatAttachments
                  v-if="attachments.length"
                  :attachments="attachments"
                  :previews="previews"
                  removable
                  :disabled="busy"
                  class="mb-3!"
                  @remove="removeAttachment"
                />
                <div v-if="uploadProgress" class="mb-2 text-xs text-accent" role="status">
                  {{ uploadProgress }}
                </div>
                <SkillTextarea
                  ref="textarea"
                  v-model="draft"
                  :skills="skills"
                  aria-label="Message"
                  :placeholder="responding ? 'Add a follow-up…' : skills.length ? 'Message your agent… Type $ for skills' : 'Message your agent…'"
                  rows="2"
                  maxlength="50000"
                  @keydown="key"
                />
                <div class="flex items-center justify-between gap-2 pt-2">
                  <div class="flex min-w-0 flex-1 items-center gap-1">
                    <input
                      ref="fileInput"
                      type="file"
                      multiple
                      class="hidden"
                      aria-label="Attach files"
                      :disabled="busy"
                      @change="pickFiles"
                    >
                    <button
                      type="button"
                      :class="iconButton"
                      aria-label="Add images or files"
                      title="Add images or files · up to 8 files, 10 MB each"
                      :disabled="busy || attachments.length >= 8"
                      @click="fileInput?.click()"
                    >
                      <Icon :name="Paperclip" :size="18" />
                    </button>
                    <AssistantPicker
                      v-model:provider="chosenProvider"
                      v-model:model="model"
                      v-model:reasoning="reasoning"
                      :inherit="inheritAgentModel"
                      :default-model="inheritAgentModel ? selectedAgent?.model : ''"
                      :default-reasoning="inheritAgentModel ? selectedAgent?.reasoning : ''"
                      :switching="switchingProvider"
                      :disabled="busy"
                    />
                  </div>
                  <!-- As on Android: steer while typing during a reply, stop when the field is empty, otherwise send. -->
                  <div class="flex shrink-0 items-center gap-1">
                    <button
                      v-if="active && !editing && hasDraft"
                      type="button"
                      class="round-action bg-soft text-accent"
                      aria-label="Steer now"
                      title="Send into the current turn (Alt + Enter)"
                      :disabled="busy || !canSend || switchingProvider"
                      @click="send('steer')"
                    >
                      <Icon :name="Zap" :size="16" />
                    </button>
                    <button
                      v-if="active && !editing && !hasDraft"
                      type="button"
                      class="round-action border border-line text-coral"
                      aria-label="Stop response"
                      title="Stop response and pause queue"
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
  </div>
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
