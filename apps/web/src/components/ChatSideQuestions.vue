<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { ChatSideQuestion } from '../../../../packages/contracts/chats'
import {
  computed,
  nextTick,
  ref,
  watch,
} from 'vue'
import { providers } from '../../../../packages/contracts/accounts'
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
  provider?: Provider | null
}>()
const emit = defineEmits<{
  /** Follow a side question back to the reply it is about. */
  reveal: [itemId: string]
}>()

const open = ref(false)
const text = ref('')
const quote = ref('')
const itemId = ref('')
/** Shows only the side questions about one reply. */
const focus = ref('')
const busy = ref(false)
const error = ref('')
const field = ref<HTMLTextAreaElement>()
let submission: { id: string, serialized: string } | undefined

// Answers that arrived while the panel was closed show above the composer until read.
const seen = ref(new Set<string>())
let known = ''
watch(() => [props.chatId, props.questions] as const, ([chatId, questions]) => {
  if (known !== chatId) {
    known = chatId
    seen.value = new Set(questions.filter(question => question.status !== 'pending').map(question => question.id))
    open.value = false
    text.value = ''
    quote.value = ''
    itemId.value = ''
    focus.value = ''
    error.value = ''
    submission = undefined
  }

  if (open.value)
    markSeen()
}, { immediate: true })

function markSeen() {
  seen.value = new Set([...seen.value, ...props.questions.filter(question => question.status !== 'pending').map(question => question.id)])
}

const pending = computed(() => props.questions.filter(question => question.status === 'pending'))
const unread = computed(() => props.questions.filter(question => question.status !== 'pending' && question.status !== 'cancelled' && !seen.value.has(question.id)))
const shown = computed(() => props.questions.filter(question => !focus.value || question.itemId === focus.value).toReversed())
const engine = computed(() => props.provider ? providers[props.provider].label : 'The agent')

async function ask(about: { itemId: string, quote: string }) {
  itemId.value = about.itemId
  quote.value = about.quote
  focus.value = ''
  error.value = ''
  show()
  await nextTick()
  field.value?.focus()
}

function show(item = '') {
  focus.value = item
  open.value = true
  markSeen()
}

async function send() {
  const question = text.value.trim()
  if (!question || busy.value || !props.canAsk)
    return
  busy.value = true
  error.value = ''
  try {
    const body = { text: question, quote: quote.value, itemId: itemId.value }
    const serialized = JSON.stringify({ chatId: props.chatId, ...body })
    if (submission?.serialized !== serialized)
      submission = { id: crypto.randomUUID(), serialized }
    await api(`/chats/${props.chatId}/side-questions`, { method: 'POST', body: JSON.stringify({ id: submission.id, ...body }) })
    submission = undefined
    text.value = ''
    quote.value = ''
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}

async function withdraw(question: ChatSideQuestion) {
  error.value = ''
  try {
    await api(`/chats/${props.chatId}/side-questions/${question.id}/cancel`, { method: 'POST' })
  }
  catch (e) { error.value = (e as Error).message }
}

function reveal(question: ChatSideQuestion) {
  if (!question.itemId)
    return
  open.value = false
  emit('reveal', question.itemId)
}

defineExpose({ ask, show })
</script>

<template>
  <div v-if="pending.length || unread.length" class="mb-3 flex items-center gap-3 rounded-xl border border-line bg-surface px-3 py-2.5" role="status">
    <span class="grid size-8 shrink-0 place-items-center rounded-lg bg-accent/10 text-accent"><Icon :name="MessageSquareQuote" :size="16" /></span>
    <button class="min-w-0 flex-1 text-left" aria-label="Open side questions" @click="show()">
      <span class="mb-0.5 block text-[10px] font-semibold text-accent">Side {{ pending.length + unread.length > 1 ? 'questions' : 'question' }} · {{ pending.length ? `${engine} is answering` : 'Answer ready' }}</span>
      <span class="block truncate text-xs font-medium">{{ (pending[0] ?? unread[0])!.text }}</span>
    </button>
    <UiButton size="small" @click="show()">
      Open
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
        {{ engine }} answers from the conversation so far, without tools, while it keeps working. Side questions never steer the reply and are not added to the conversation.
      </p>
      <form v-if="canAsk" class="space-y-3" @submit.prevent="send">
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
            ref="field"
            v-model="text"
            rows="3"
            maxlength="4000"
            class="mt-2! max-h-48 resize-y"
            placeholder="Why did you choose this approach?"
            @keydown.enter.exact.prevent="send"
          />
        </label>
        <div class="flex justify-end">
          <UiButton type="submit" variant="primary" :disabled="busy || !text.trim()">
            <Icon :name="Send" :size="15" />{{ busy ? 'Asking…' : 'Ask' }}
          </UiButton>
        </div>
      </form>
      <UiNotice v-else :icon="MessageSquareQuote">
        {{ provider === 'opencode' ? 'OpenCode cannot answer side questions in Cairn yet. Ask in the conversation instead.' : 'Side questions are answered while Codex or Claude Code works on a reply.' }}
      </UiNotice>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <div v-if="focus" class="flex items-center justify-between gap-3 text-xs text-muted">
        About one reply
        <button type="button" class="underline hover:text-ink" @click="focus = ''">
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
            <p v-if="question.status === 'pending'" class="flex items-center gap-2 text-xs text-muted" role="status">
              <span class="size-1.5 animate-pulse rounded-full bg-accent" />Answering…
            </p>
            <Markdown v-else-if="question.status === 'answered'" :content="question.answer ?? ''" />
            <p v-else-if="question.status === 'failed'" class="text-xs text-danger">
              {{ question.error }}
            </p>
            <p v-else class="text-xs text-muted">
              Withdrawn
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
