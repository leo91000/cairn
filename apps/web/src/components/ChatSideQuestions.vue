<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { ChatSideQuestion } from '../../../../packages/contracts/chats'
import {
  computed,
  ref,
  watch,
} from 'vue'
import { providers } from '../../../../packages/contracts/accounts'
import { askChatSideQuestion } from '../../../../packages/contracts/generated/askChatSideQuestion'
import { cancelChatSideQuestion } from '../../../../packages/contracts/generated/cancelChatSideQuestion'
import { api } from '../api'
import { MessageSquareQuote, Send, X } from '../icons'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiNotice from './UiNotice.vue'

const props = defineProps<{
  chatId: string
  questions: ChatSideQuestion[]
  /** The running reply can answer a side question now. */
  canAsk: boolean
  /** Why a side question cannot be asked now, from the server. */
  reason?: string | null
  provider?: Provider | null
}>()
const emit = defineEmits<{
  /** Follow a side question back to the reply it is about. */
  reveal: [reply: { itemId: string, replyEventId: number | null }]
}>()

const open = ref(false)
const text = ref('')
const quote = ref('')
const itemId = ref('')
const replyEventId = ref<number | null>(null)
/** Shows only the side questions about one reply. */
const focus = ref('')
const focusEvents = ref<number[]>([])
const busy = ref(false)
const error = ref('')
/** The dialog focuses the question field when it opens to ask. */
const asking = ref(false)
let submission: { id: string, serialized: string } | undefined
let draftRevision = 0
watch([text, quote, itemId, replyEventId, focus], () => {
  draftRevision++
}, { flush: 'sync' })

// Answers that arrived while the panel was closed show above the composer until read.
const seen = ref(new Set<string>())
let known = ''
watch(() => [props.chatId, props.questions] as const, ([chatId, questions]) => {
  if (known !== chatId) {
    draftRevision++
    known = chatId
    seen.value = new Set(questions.filter(question => question.status !== 'pending').map(question => question.id))
    open.value = false
    text.value = ''
    quote.value = ''
    itemId.value = ''
    replyEventId.value = null
    focus.value = ''
    focusEvents.value = []
    error.value = ''
    submission = undefined
  }

  if (open.value)
    markSeen()
}, { immediate: true })

const pending = computed(() => props.questions.filter(question => question.status === 'pending'))
const unread = computed(() => props.questions.filter(question => question.status !== 'pending' && question.status !== 'cancelled' && !seen.value.has(question.id)))
const shown = computed(() => props.questions.filter((question) => {
  if (!focus.value)
    return true
  if (question.itemId !== focus.value)
    return false
  if (!question.replyEventId)
    return true
  const id = Number(question.replyEventId)
  const [start, end] = focusEvents.value
  return start !== undefined && end !== undefined && id >= start && id <= end
}).toReversed())

function markSeen() {
  seen.value = new Set([...seen.value, ...shown.value.filter(question => question.status !== 'pending').map(question => question.id)])
}

const engine = computed(() => props.provider ? providers[props.provider].label : 'The agent')
const status = computed(() => {
  if (pending.value.length)
    return `${engine.value} is answering`
  if (!unread.value.length)
    return 'Saved'
  return unread.value.some(question => question.status === 'answered') ? 'Answer ready' : 'Not answered'
})

function ask(about: { itemId: string, replyEventId: number | null, quote: string }) {
  draftRevision++
  itemId.value = about.itemId
  replyEventId.value = about.replyEventId
  quote.value = about.quote
  error.value = ''
  open.value = true
  asking.value = true
  focus.value = ''
  markSeen()
}

/** Opens the panel, on the side questions about one reply when `item` is given. */
function show(reply?: { itemId: string, replyEventId: number | null, replyEventIds: number[] }) {
  draftRevision++
  const item = reply?.itemId ?? ''
  // Questions asked from a reply's panel are about that reply.
  if (item && (item !== itemId.value || reply?.replyEventId !== replyEventId.value)) {
    itemId.value = item
    quote.value = ''
  }

  if (reply)
    replyEventId.value = reply.replyEventId

  focus.value = item
  focusEvents.value = reply?.replyEventIds ?? []
  open.value = true
  asking.value = false
  markSeen()
}

/** Each question's status, shown while it waits or was withdrawn and always read out. */
function spoken(question: ChatSideQuestion) {
  return {
    pending: 'Answering…',
    answered: 'Answered',
    failed: 'Not answered',
    cancelled: 'Withdrawn',
  }[question.status]
}

function key(event: KeyboardEvent) {
  if (event.shiftKey || event.isComposing)
    return
  // Touch keyboards keep Enter for newlines, as in the conversation composer.
  if (window.matchMedia('(pointer: coarse)').matches && !event.ctrlKey && !event.metaKey)
    return
  event.preventDefault()
  void send()
}

async function send() {
  const question = text.value.trim()
  if (!question || busy.value || !props.canAsk)
    return
  busy.value = true
  error.value = ''
  const chatId = props.chatId
  const revision = draftRevision
  try {
    const body = {
      text: question,
      quote: quote.value,
      itemId: itemId.value,
      replyEventId: replyEventId.value?.toString() ?? '',
    }
    const serialized = JSON.stringify({ chatId, ...body })
    if (submission?.serialized !== serialized)
      submission = { id: crypto.randomUUID(), serialized }
    const sent = submission
    await askChatSideQuestion(api, { chatId, question: { id: sent.id, ...body } })
    if (submission === sent)
      submission = undefined
    if (props.chatId === chatId && draftRevision === revision) {
      text.value = ''
      quote.value = ''
      itemId.value = focus.value
      if (!focus.value)
        replyEventId.value = null
    }
  }
  catch (e) {
    if (props.chatId === chatId && draftRevision === revision)
      error.value = (e as Error).message
  }
  finally { busy.value = false }
}

async function withdraw(question: ChatSideQuestion) {
  error.value = ''
  try {
    await cancelChatSideQuestion(api, { chatId: props.chatId, sideQuestionId: question.id })
  }
  catch (e) { error.value = (e as Error).message }
}

/** Reopens the panel when the reply a question is about is not loaded. */
function unreachable(reply: { itemId: string, replyEventId: number | null }) {
  show({ ...reply, replyEventIds: reply.replyEventId ? [reply.replyEventId, reply.replyEventId] : [] })
  error.value = 'This reply is not in the loaded conversation. Scroll up to load earlier messages, then try again.'
}

function reveal(question: ChatSideQuestion) {
  if (!question.itemId)
    return
  open.value = false
  emit('reveal', { itemId: question.itemId, replyEventId: Number(question.replyEventId) || null })
}

defineExpose({ ask, show, unreachable })
</script>

<template>
  <p class="sr-only" role="status">
    {{ pending.length || unread.length ? `Side ${pending.length + unread.length > 1 ? 'questions' : 'question'} · ${status}` : '' }}
  </p>
  <div v-if="questions.length || canAsk" class="mb-3 flex items-center gap-3 rounded-xl border border-line bg-surface px-3 py-2.5">
    <span class="grid size-8 shrink-0 place-items-center rounded-lg bg-accent/10 text-accent"><Icon :name="MessageSquareQuote" :size="16" /></span>
    <button class="min-w-0 flex-1 text-left" @click="questions.length ? show() : ask({ itemId: '', replyEventId: null, quote: '' })">
      <span class="mb-0.5 block text-[10px] font-semibold text-accent">{{ questions.length ? `Side ${questions.length > 1 ? 'questions' : 'question'} · ${status}` : 'Ask a side question' }}</span>
      <span v-if="questions.length" class="block truncate text-xs font-medium">{{ (pending[0] ?? unread[0] ?? questions.at(-1))!.text }}</span>
    </button>
    <UiButton size="small" @click="questions.length ? show() : ask({ itemId: '', replyEventId: null, quote: '' })">
      {{ questions.length ? 'Open' : 'Ask' }}
    </UiButton>
  </div>
  <Modal
    v-if="open"
    title="Side questions"
    drawer
    @close="open = false"
  >
    <div class="space-y-5 p-6 phone:p-4">
      <p class="text-xs leading-relaxed text-muted">
        {{ engine }} answers from the conversation so far and denies requested tools while it keeps working. Side questions never steer the reply and are not added to the conversation.
      </p>
      <p v-if="provider === 'claude'" class="text-xs leading-relaxed text-muted">
        Claude denies attempted tools, but an operator’s managed-policy hooks can run before the denial and may have side effects. Withdrawing cannot undo effects already performed.
      </p>
      <form v-if="canAsk" class="space-y-3" @submit.prevent="send">
        <button
          v-if="itemId"
          type="button"
          class="text-xs text-accent underline"
          @click="ask({ itemId: '', replyEventId: null, quote: '' })"
        >
          Ask a general question instead
        </button>
        <blockquote v-if="quote" class="relative rounded-lg border-l-3 border-accent/50 bg-variant/60 py-2 pr-9 pl-3 text-xs leading-relaxed whitespace-pre-wrap break-words text-muted">
          {{ quote }}
          <button
            type="button"
            class="absolute top-1.5 right-1.5 grid size-6 place-items-center rounded text-muted hover:bg-hover hover:text-ink"
            aria-label="Remove quoted passage"
            @click="quote = ''"
          >
            <Icon :name="X" :size="12" />
          </button>
        </blockquote>
        <label class="block text-xs text-muted">
          {{ quote ? 'Your question about this passage' : itemId ? 'Your question about this reply' : 'Your question' }}
          <textarea
            v-model="text"
            rows="3"
            maxlength="4000"
            class="mt-2! max-h-48 resize-y"
            placeholder="Why did you choose this approach?"
            :autofocus="asking"
            @keydown.enter="key"
          />
        </label>
        <div class="flex justify-end">
          <UiButton type="submit" variant="primary" :disabled="busy || !text.trim()">
            <Icon :name="Send" :size="15" />{{ busy ? 'Asking…' : 'Ask' }}
          </UiButton>
        </div>
      </form>
      <UiNotice v-else :icon="MessageSquareQuote">
        {{ reason || 'Side questions are answered while Claude Code works on a reply.' }}
      </UiNotice>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <div v-if="focus" class="flex items-center justify-between gap-3 text-xs text-muted">
        About one reply
        <button type="button" class="underline hover:text-ink" @click="focus = ''; markSeen()">
          Show all
        </button>
      </div>
      <ol class="space-y-3" aria-label="Asked side questions">
        <li v-for="question in shown" :key="question.id" class="side-question rounded-xl border border-line p-3.5">
          <blockquote v-if="question.quote" class="mb-2 line-clamp-3 border-l-2 border-line pl-2.5 text-xs text-muted">
            {{ question.quote }}
          </blockquote>
          <p class="text-sm font-medium break-words whitespace-pre-wrap">
            {{ question.text }}
          </p>
          <div class="mt-2.5 text-sm">
            <!-- One persistent status per question, read out when the answer arrives. -->
            <p
              :class="question.status === 'pending' || question.status === 'cancelled' ? 'flex items-center gap-2 text-xs text-muted' : 'sr-only'"
              role="status"
            >
              <span v-if="question.status === 'pending'" class="size-1.5 animate-pulse rounded-full bg-accent" />{{ spoken(question) }}
            </p>
            <Markdown v-if="question.status === 'answered'" :content="question.answer ?? ''" />
            <p v-else-if="question.status === 'failed'" class="text-xs text-danger">
              {{ question.error }}
            </p>
          </div>
          <div class="mt-2.5 flex items-center justify-between gap-3 text-xs text-muted">
            <button
              v-if="question.itemId"
              type="button"
              class="underline hover:text-ink"
              @click="reveal(question)"
            >
              Show reply
            </button>
            <span v-else />
            <button
              v-if="question.status === 'pending'"
              type="button"
              class="underline hover:text-ink"
              @click="withdraw(question)"
            >
              Withdraw
            </button>
          </div>
        </li>
      </ol>
      <p v-if="!shown.length" class="text-xs text-muted">
        No side questions yet.
      </p>
    </div>
  </Modal>
</template>
