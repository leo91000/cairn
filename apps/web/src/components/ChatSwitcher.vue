<script setup lang="ts">
import type { ChatView } from '../../../../packages/contracts/chats'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  useId,
  watch,
} from 'vue'
import { api, ApiError, state } from '../api'
import {
  Clock,
  MessageCircle,
  MoreHorizontal,
  Plus,
  Search,
  X,
} from '../icons'
import { iconButton } from '../ui'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ chats: ChatView[], selected?: string, anchor?: HTMLElement }>()
const emit = defineEmits<{ close: [], create: [] }>()
const dialog = ref<HTMLDialogElement>()
const query = ref('')
const view = ref<'active' | 'trash'>('active')
const views = [
  { value: 'active', label: 'Active conversations' },
  { value: 'trash', label: 'Trash' },
]
const other = ref<ChatView[]>([])
const loading = ref(false)
const error = ref('')
const confirmation = ref<ChatView>()
const deleting = ref(false)
const conversations = computed(() => view.value === 'active' ? props.chats : other.value)
let request = 0

async function load() {
  const sequence = ++request
  error.value = ''
  if (view.value === 'active') {
    loading.value = false
    return
  }

  loading.value = true
  try {
    const result = await api<ChatView[]>(`/chats?view=${view.value}`)
    if (sequence === request)
      other.value = result
  }
  catch (e) {
    if (sequence === request)
      error.value = (e as Error).message
  }
  finally {
    if (sequence === request)
      loading.value = false
  }
}

watch([view, () => props.chats], load)

async function remove(chat: ChatView, confirm = false) {
  if (deleting.value)
    return
  deleting.value = true
  error.value = ''
  try {
    await api(`/chats/${chat.id}`, { method: 'DELETE', body: JSON.stringify({ confirm }) })
    const draftScope = state.installationId ? `${state.installationId}:` : ''
    sessionStorage.removeItem(`cairn-chat-draft:${draftScope}${chat.id}`)
    confirmation.value = undefined
    await load()
  }
  catch (e) {
    if (e instanceof ApiError && e.status === 409 && !confirm)
      confirmation.value = chat
    else
      error.value = (e as Error).message
  }
  finally { deleting.value = false }
}

const titleId = useId()
const previous = document.activeElement as HTMLElement | null
const groups = computed(() => {
  const today = new Date()
  today.setHours(0, 0, 0, 0)
  const yesterday = new Date(today)
  yesterday.setDate(today.getDate() - 1)
  const filtered = conversations.value.filter(chat => `${chat.title} ${chat.agentName} ${chat.projectName ?? ''}`.toLocaleLowerCase().includes(query.value.trim().toLocaleLowerCase()))
  return [
    { title: 'Today', chats: filtered.filter(chat => chat.updatedAt >= +today) },
    { title: 'Yesterday', chats: filtered.filter(chat => chat.updatedAt >= +yesterday && chat.updatedAt < +today) },
    { title: 'Earlier', chats: filtered.filter(chat => chat.updatedAt < +yesterday) },
  ].map(group => ({ ...group, chats: group.chats.toSorted((a, b) => b.updatedAt - a.updatedAt) })).filter(group => group.chats.length)
})

function position() {
  const bounds = props.anchor?.getBoundingClientRect()
  if (!dialog.value || !bounds)
    return
  dialog.value.style.setProperty('--switcher-left', `${Math.max(14, Math.min(bounds.left, window.innerWidth - 394))}px`)
  dialog.value.style.setProperty('--switcher-top', `${bounds.bottom + 12}px`)
}

onMounted(() => {
  position()
  dialog.value?.showModal()
  window.addEventListener('resize', position)
})
onBeforeUnmount(() => {
  window.removeEventListener('resize', position)
  dialog.value?.close()
  const target = props.anchor ?? previous
  target?.focus()
})

function time(timestamp: number) {
  const date = new Date(timestamp)
  return date.toLocaleDateString() === new Date().toLocaleDateString()
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString([], { month: 'short', day: 'numeric' })
}
</script>

<template>
  <Teleport to="body">
    <dialog
      ref="dialog"
      class="chat-switcher"
      :aria-labelledby="titleId"
      @cancel.prevent="emit('close')"
      @click="event => { if (event.target === dialog) emit('close') }"
    >
      <div class="switcher-content flex min-h-0 flex-col p-5 phone:p-4">
        <div aria-hidden="true" class="mx-auto mb-4 hidden h-1 w-9 shrink-0 rounded-full bg-muted/40 phone:block" />
        <header class="mb-4 flex shrink-0 items-center justify-between gap-3">
          <h2 :id="titleId" class="flex items-center gap-2">
            Conversations <span class="text-xs font-normal text-muted">{{ conversations.length }}</span>
          </h2>
          <button :class="iconButton" aria-label="Close conversations" @click="emit('close')">
            <Icon :name="X" :size="19" />
          </button>
        </header>
        <button class="mb-4 flex min-h-11 shrink-0 items-center justify-center gap-2 rounded-lg bg-accent px-3 py-2 text-xs font-semibold text-canvas" @click="emit('create')">
          <Icon :name="Plus" :size="17" />New conversation
        </button>
        <div class="mb-3 flex shrink-0 items-center justify-end">
          <VirtualSelect
            :model-value="view"
            label="Conversation view"
            :options="views"
            compact
            hide-label
            @update:model-value="view = $event as 'active' | 'trash'"
          />
        </div>
        <p v-if="error" role="alert" class="mb-3 text-xs text-danger">
          {{ error }}
        </p>
        <p v-if="loading" role="status" class="mb-3 text-xs text-muted">
          Loading…
        </p>
        <label class="relative mb-4 block shrink-0">
          <Icon :name="Search" :size="16" class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-muted" />
          <input
            v-model="query"
            type="search"
            autofocus
            aria-label="Search conversations"
            placeholder="Search conversations…"
            class="pl-10! text-sm! phone:text-base!"
          >
        </label>
        <nav aria-label="Recent conversations" class="min-h-0 flex-1 overflow-auto overscroll-contain px-1 [scrollbar-width:thin]">
          <p v-if="!conversations.length && !loading" class="px-2 py-6 text-center text-xs text-muted">
            Your conversations will appear here.
          </p>
          <p v-else-if="!groups.length" role="status" class="px-2 py-6 text-center text-xs text-muted">
            No conversations match your search.
          </p>
          <section v-for="group in groups" :key="group.title" class="mb-4">
            <h3 class="mb-2 px-2 text-[11px]! font-normal text-muted">
              {{ group.title }}
            </h3>
            <div v-for="chat in group.chats" :key="chat.id" class="flex items-start gap-1">
              <RouterLink
                :to="`/chats/${chat.id}`"
                :aria-current="chat.id === selected ? 'page' : undefined"
                class="conversation-choice mb-1 min-w-0 flex flex-1 gap-3 rounded-lg border border-transparent px-3 py-3 hover:bg-hover"
                :class="chat.id === selected ? 'border-accent/25! bg-accent/10' : ''"
                @click="emit('close')"
              >
                <div class="min-w-0 flex-1">
                  <span class="line-clamp-2 text-xs font-semibold leading-relaxed wrap-anywhere">{{ chat.title }}</span>
                  <span class="mt-1.5 block text-[11px] leading-relaxed text-muted wrap-anywhere">{{ chat.agentName }}<template v-if="chat.projectName"> · {{ chat.projectName }}</template></span>
                  <span v-if="chat.pendingQuestions" class="mt-2 inline-block rounded-md bg-warning-surface px-2 py-1 text-[10px] text-warning">{{ chat.pendingQuestions }} awaiting answer</span>
                  <span v-else-if="chat.status === 'running' || chat.status === 'queued'" class="mt-2 flex items-center gap-1.5 text-[10px] text-accent"><Icon :name="Clock" :size="12" />{{ chat.status === 'queued' ? 'Waiting' : 'Working' }}</span>
                  <span v-else-if="chat.status === 'failed' || chat.status === 'interrupted'" class="mt-2 block text-[10px] text-danger">{{ chat.status === 'failed' ? 'Response failed' : 'Interrupted' }}</span>
                </div>
                <time :datetime="new Date(chat.updatedAt).toISOString()" class="shrink-0 pt-1 text-[10px] text-muted">{{ time(chat.updatedAt) }}</time>
              </RouterLink>
              <details v-if="view !== 'trash'" class="relative shrink-0 pt-3">
                <summary :aria-label="`Actions for ${chat.title}`" class="cursor-pointer list-none rounded-md p-1.5 text-muted hover:bg-hover">
                  <Icon :name="MoreHorizontal" :size="18" />
                </summary>
                <button class="absolute right-0 z-10 rounded-lg border border-line bg-raised px-4 py-2 text-xs text-danger shadow-lg" :disabled="deleting" @click="remove(chat)">
                  Delete
                </button>
              </details>
            </div>
          </section>
        </nav>
        <p class="mt-3 flex shrink-0 items-center gap-2 border-t border-line px-1 pt-3 text-[10px] text-muted">
          <Icon :name="MessageCircle" :size="13" />Switch conversations without losing your draft.
        </p>
      </div>
    </dialog>
    <Modal v-if="confirmation" title="Stop and delete conversation?" @close="confirmation = undefined">
      <p class="mb-5 text-sm text-muted">
        This stops the agent, cancels pending messages and questions, and moves the conversation to the trash for 30 days. Public links are revoked immediately.
      </p>
      <div class="flex justify-end gap-3">
        <UiButton :disabled="deleting" @click="confirmation = undefined">
          Cancel
        </UiButton>
        <UiButton :disabled="deleting" variant="primary" @click="remove(confirmation!, true)">
          Stop and delete
        </UiButton>
      </div>
    </Modal>
  </Teleport>
</template>

<style scoped>
.chat-switcher {
  position: fixed;
  inset: var(--switcher-top, 100px) auto auto var(--switcher-left, 24px);
  margin: 0;
  padding: 0;
  width: 380px;
  max-width: calc(100vw - 28px);
  max-height: min(720px, calc(100dvh - var(--switcher-top, 100px) - 20px));
  border: 1px solid var(--color-line);
  border-radius: 16px;
  background: var(--color-raised);
  color: var(--color-ink);
  box-shadow: 0 24px 80px #0003;
  overflow: hidden;
}
.switcher-content { max-height: inherit; }
.chat-switcher::backdrop { background: #0002; }
@media (max-width: 640px) {
  .chat-switcher {
    inset: auto 0 0;
    width: 100%;
    max-width: none;
    max-height: min(82dvh, calc(100dvh - env(safe-area-inset-top) - 16px));
    border-radius: 22px 22px 0 0;
    padding-bottom: env(safe-area-inset-bottom);
  }
  .switcher-content { max-height: calc(min(82dvh, 100dvh - env(safe-area-inset-top) - 16px) - env(safe-area-inset-bottom)); }
  .chat-switcher::backdrop { background: #0a091080; backdrop-filter: blur(3px); }
}
</style>
