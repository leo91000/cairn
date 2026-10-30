<script setup lang="ts">
import type { ChatQuestion } from '../../../../packages/contracts/chats'
import { computed, ref, watch } from 'vue'
import { validQuestionAnswer } from '../../../../packages/contracts/chats'
import { answerChatQuestion } from '../../../../packages/contracts/generated/answerChatQuestion'
import { cancelChatQuestion } from '../../../../packages/contracts/generated/cancelChatQuestion'
import { api, state } from '../api'
import { ChevronDown, MessageCircleQuestion, Send } from '../icons'
import { exclusiveSuspensionLabel } from '../run-status'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{
  questions: ChatQuestion[]
  active: boolean
  suspended?: boolean
  highlighted?: string
  // Only people who may use the agent answer its questions (ADR-0075).
  readOnly?: boolean
}>()
const emit = defineEmits<{ answered: [] }>()
const pending = computed(() => props.questions.filter(question => question.status === 'pending'))
const open = ref(false)
const selected = ref('')
const question = computed(() => pending.value.find(question => question.id === selected.value) ?? pending.value[0])
const fields = computed(() => {
  const fields = question.value?.fields
  if (!fields?.length || fields.some(field => !['single', 'multiple'].includes(field.mode) || !Array.isArray(field.options) || field.options.some(option => !option.id) || new Set(field.options.map(option => option.id)).size !== field.options.length))
    return null
  return fields
})
const answers = ref<Record<string, Record<string, string[]>>>({})
const custom = ref<Record<string, Record<string, string>>>({})
const busy = ref(false)
const error = ref('')
let submission: { id: string, serialized: string } | undefined
let dismissedHighlight: string | undefined
watch(() => props.readOnly, (readOnly) => {
  if (readOnly) {
    open.value = false
    dismissedHighlight = props.highlighted
  }
}, { immediate: true })
watch(question, (value) => {
  if (!value) {
    open.value = false
    return
  }

  answers.value[value.id] ??= {}
  custom.value[value.id] ??= {}
}, { immediate: true })
watch([() => props.highlighted, () => pending.value.some(question => question.id === props.highlighted)], ([highlighted, available], [previousHighlighted]) => {
  if (highlighted !== previousHighlighted)
    dismissedHighlight = undefined

  if (props.readOnly) {
    dismissedHighlight = highlighted
    return
  }

  if (highlighted && available && highlighted !== dismissedHighlight) {
    selected.value = highlighted
    open.value = true
  }
}, { immediate: true })
const ready = computed(() => fields.value?.every(field => validQuestionAnswer(field, {
  selected: answers.value[question.value!.id]?.[field.id] ?? [],
  comment: custom.value[question.value!.id]?.[field.id] ?? '',
})))

function toggle(field: ChatQuestion['fields'][number], id: string) {
  const current = question.value
  if (!current)
    return

  const selected = answers.value[current.id]![field.id] ?? []
  answers.value[current.id]![field.id] = field.mode === 'single'
    ? [id]
    : selected.includes(id) ? selected.filter(value => value !== id) : [...selected, id]
}

function selectionHint(field: ChatQuestion['fields'][number]) {
  if (field.mode === 'single')
    return 'Choose one option, or answer using text only'

  const minimum = field.minChoices ?? 1
  const maximum = field.maxChoices ?? field.options.length
  const range = minimum === maximum ? `${minimum}` : `${minimum}–${maximum}`
  return `Choose ${range} options, or answer using text only`
}

function selectionFeedback(field: ChatQuestion['fields'][number]) {
  const count = answers.value[question.value!.id]?.[field.id]?.length ?? 0
  const minimum = field.minChoices ?? 1
  const maximum = field.mode === 'single' ? 1 : (field.maxChoices ?? field.options.length)
  if (count > maximum)
    return `${count} selected · Remove ${count - maximum}`
  if (count > 0 && count < minimum)
    return `${count} selected · Choose ${minimum - count} more`
  return `${count} selected`
}

async function send() {
  const current = question.value
  if (props.readOnly || !current || !fields.value || !ready.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    const values = Object.fromEntries(fields.value.map(field => [field.id, {
      selected: field.options.filter(option => answers.value[current.id]?.[field.id]?.includes(option.id)).map(option => option.id),
      comment: custom.value[current.id]?.[field.id]?.trim() ?? '',
    }]))
    const serialized = JSON.stringify({ questionId: current.id, answers: values })
    if (submission?.serialized !== serialized)
      submission = { id: crypto.randomUUID(), serialized }
    await answerChatQuestion(api, { chatId: current.chatId, questionId: current.id, answer: { id: submission.id, answers: values } })
    navigator.serviceWorker?.controller?.postMessage({ type: 'question-answered', installationId: state.installationId, questionId: current.id })
    delete answers.value[current.id]
    delete custom.value[current.id]
    submission = undefined
    emit('answered')
    open.value = false
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}

async function withdraw() {
  const current = question.value
  if (props.readOnly || !current || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    await cancelChatQuestion(api, { chatId: current.chatId, questionId: current.id })
    navigator.serviceWorker?.controller?.postMessage({ type: 'question-answered', installationId: state.installationId, questionId: current.id })
    delete answers.value[current.id]
    delete custom.value[current.id]
    submission = undefined
    emit('answered')
    open.value = false
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
</script>

<template>
  <div v-if="question" class="mb-3 flex items-center gap-3 rounded-xl border border-accent/30 bg-accent/8 px-3 py-3" role="status">
    <span class="grid size-8 shrink-0 place-items-center rounded-lg bg-accent text-on-brand"><Icon :name="MessageCircleQuestion" :size="16" /></span>
    <div v-if="readOnly" class="min-w-0 flex-1">
      <span class="mb-0.5 block text-[10px] font-semibold text-accent">{{ pending.length > 1 ? `${pending.length} questions` : 'Question' }} · Waiting for someone with access to this agent</span>
      <span class="block truncate text-xs font-medium">{{ question.fields[0]?.title }}</span>
    </div>
    <template v-else>
      <button class="min-w-0 flex-1 text-left" aria-label="Answer pending questions" @click="open = true">
        <span class="mb-0.5 block text-[10px] font-semibold text-accent">{{ pending.length > 1 ? `${pending.length} questions` : 'Your input' }} · {{ question.blocking && active ? 'Waiting for you' : 'Answer when ready' }}</span>
        <span class="block truncate text-xs font-medium">{{ question.fields[0]?.title }}</span>
      </button>
      <UiButton size="small" @click="open = true">
        Answer
      </UiButton>
    </template>
  </div>
  <Modal v-if="open && question && !readOnly" title="Your input" @close="open = false">
    <form class="space-y-5 p-6 phone:p-4" @submit.prevent="send">
      <div class="flex items-center justify-between gap-3">
        <span class="inline-flex items-center gap-1.5 rounded-full bg-accent/10 px-2.5 py-1 text-[11px] font-medium text-accent"><span class="size-1.5 rounded-full bg-accent" />{{ question.blocking && active ? 'Agent is waiting' : active && suspended ? exclusiveSuspensionLabel : active ? 'Agent keeps working' : 'Continue the conversation' }}</span>
        <div v-if="pending.length > 1" class="flex items-center gap-2 text-xs text-muted">
          <button
            type="button"
            aria-label="Previous question"
            class="p-1"
            @click="selected = pending[(pending.indexOf(question) + pending.length - 1) % pending.length]!.id"
          >
            <Icon :name="ChevronDown" :size="14" class="rotate-90" />
          </button>
          {{ pending.indexOf(question) + 1 }} / {{ pending.length }}
          <button
            type="button"
            aria-label="Next question"
            class="p-1"
            @click="selected = pending[(pending.indexOf(question) + 1) % pending.length]!.id"
          >
            <Icon :name="ChevronDown" :size="14" class="-rotate-90" />
          </button>
        </div>
      </div>
      <UiAlert v-if="!fields">
        This question uses an unsupported format. Update the Cairn installation, then ask the agent to ask again. You can withdraw this question below.
      </UiAlert>
      <fieldset v-for="field in fields ?? []" :key="`${question.id}:${field.id}`" class="min-w-0 space-y-2.5">
        <legend class="mb-3 text-base! leading-relaxed! font-semibold! text-ink! break-words">
          {{ field.title }}
        </legend>
        <label
          v-for="option in field.options"
          :key="option.id"
          class="flex! flex-row! cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition-colors hover:border-accent/50"
          :class="answers[question.id]?.[field.id]?.includes(option.id) ? 'border-accent bg-accent/8' : 'border-line bg-surface'"
        >
          <input
            :checked="answers[question.id]?.[field.id]?.includes(option.id) ?? false"
            :type="field.mode === 'multiple' ? 'checkbox' : 'radio'"
            :name="`${question.id}:${field.id}`"
            :value="option.id"
            class="mt-1! size-4! shrink-0 p-0! accent-accent"
            @change="toggle(field, option.id)"
          >
          <span class="min-w-0 break-words"><span class="block text-sm font-medium">{{ option.label }} <span v-if="option.recommended" class="ml-1 inline-flex rounded-full bg-accent/10 px-2 py-0.5 text-[10px] font-semibold text-accent">Recommended</span></span><span v-if="option.description" class="mt-1 block text-xs leading-relaxed text-muted">{{ option.description }}</span></span>
        </label>
        <div v-if="field.options.length" class="flex items-center justify-between gap-2 text-xs text-muted">
          <span>{{ selectionHint(field) }}</span>
          <button
            v-if="answers[question.id]?.[field.id]?.length"
            type="button"
            class="shrink-0 underline"
            @click="answers[question.id]![field.id] = []"
          >
            Clear choices
          </button>
        </div>
        <p
          v-if="field.mode === 'multiple' && field.options.length"
          role="status"
          aria-live="polite"
          class="text-xs"
          :class="answers[question.id]?.[field.id]?.length && !validQuestionAnswer(field, { selected: answers[question.id]?.[field.id] ?? [], comment: '' }) ? 'text-danger' : 'text-muted'"
        >
          {{ selectionFeedback(field) }}
        </p>
        <label class="block text-xs text-muted">
          {{ field.options.length ? 'Comment or your own answer (optional with choices)' : 'Your answer' }}
          <input
            v-if="field.secret"
            v-model="custom[question.id]![field.id]"
            type="password"
            autocomplete="off"
            class="mt-2!"
            maxlength="10000"
          >
          <textarea
            v-else
            v-model="custom[question.id]![field.id]"
            rows="2"
            maxlength="10000"
            class="mt-2! max-h-40 resize-y"
            placeholder="What do you have in mind?"
          />
        </label>
      </fieldset>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <button
        type="button"
        class="text-sm text-muted underline hover:text-ink"
        :disabled="busy"
        @click="withdraw"
      >
        No longer needed
      </button>
      <div class="flex items-center justify-between gap-3 border-t border-line pt-4">
        <button type="button" class="text-xs text-muted hover:text-ink" @click="open = false">
          Answer later
        </button>
        <UiButton type="submit" variant="primary" :disabled="busy || !ready">
          <Icon :name="Send" :size="15" />{{ busy ? 'Sending…' : 'Send answer' }}
        </UiButton>
      </div>
    </form>
  </Modal>
</template>
