<script setup lang="ts">
import type { ChatQuestion } from '../../shared/chats'
import { computed, ref, watch } from 'vue'
import { api } from '../api'
import { ChevronDown, MessageCircleQuestion, Send } from '../icons'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ questions: ChatQuestion[], active: boolean, highlighted?: string }>()
const emit = defineEmits<{ answered: [] }>()
const pending = computed(() => props.questions.filter(question => question.status === 'pending'))
const open = ref(false)
const selected = ref('')
const question = computed(() => pending.value.find(question => question.id === selected.value) ?? pending.value[0])
const answers = ref<Record<string, Record<string, string>>>({})
const custom = ref<Record<string, Record<string, string>>>({})
const busy = ref(false)
const error = ref('')
let submission: { id: string, serialized: string } | undefined
watch(question, (value) => {
  if (!value) {
    open.value = false
    return
  }

  answers.value[value.id] ??= {}
  custom.value[value.id] ??= {}
}, { immediate: true })
watch(() => [props.highlighted, pending.value.length], () => {
  if (props.highlighted && pending.value.some(question => question.id === props.highlighted)) {
    selected.value = props.highlighted
    open.value = true
  }
}, { immediate: true })
const ready = computed(() => question.value?.fields.every(field => (answers.value[question.value!.id]?.[field.id] || custom.value[question.value!.id]?.[field.id])?.trim()))

async function send() {
  const current = question.value
  if (!current || !ready.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    const values = Object.fromEntries(current.fields.map(field => [field.id, [answers.value[current.id]?.[field.id] || custom.value[current.id]?.[field.id]]]))
    const serialized = JSON.stringify({ questionId: current.id, answers: values })
    if (submission?.serialized !== serialized)
      submission = { id: crypto.randomUUID(), serialized }
    await api(`/chats/${current.chatId}/questions/${current.id}/answer`, { method: 'POST', body: JSON.stringify({ id: submission.id, answers: values }) })
    navigator.serviceWorker?.controller?.postMessage({ type: 'question-answered', questionId: current.id })
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
    <span class="grid size-8 shrink-0 place-items-center rounded-lg bg-accent text-white"><Icon :name="MessageCircleQuestion" :size="16" /></span>
    <button class="min-w-0 flex-1 text-left" aria-label="Answer pending questions" @click="open = true">
      <span class="mb-0.5 block text-[10px] font-semibold text-accent">{{ pending.length > 1 ? `${pending.length} questions` : 'Your input' }} · {{ question.blocking && active ? 'Waiting for you' : 'Answer when ready' }}</span>
      <span class="block truncate text-xs font-medium">{{ question.fields[0]?.title }}</span>
    </button>
    <UiButton size="small" @click="open = true">
      Answer
    </UiButton>
  </div>
  <Modal v-if="open && question" title="Your input" @close="open = false">
    <form class="space-y-5 p-6 phone:p-4" @submit.prevent="send">
      <div class="flex items-center justify-between gap-3">
        <span class="inline-flex items-center gap-1.5 rounded-full bg-accent/10 px-2.5 py-1 text-[11px] font-medium text-accent"><span class="size-1.5 rounded-full bg-accent" />{{ question.blocking && active ? 'Agent is waiting' : active ? 'Agent keeps working' : 'Continue the conversation' }}</span>
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
      <fieldset v-for="field in question.fields" :key="`${question.id}:${field.id}`" class="min-w-0 space-y-2.5">
        <legend class="mb-3 text-base! leading-relaxed! font-semibold! text-ink! break-words">
          {{ field.title }}
        </legend>
        <label
          v-for="(option, index) in field.options"
          :key="index"
          class="flex! flex-row! cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition-colors hover:border-accent/50"
          :class="answers[question.id]?.[field.id] === option.label ? 'border-accent bg-accent/8' : 'border-line bg-surface'"
        >
          <input
            v-model="answers[question.id]![field.id]"
            type="radio"
            :name="`${question.id}:${field.id}`"
            :value="option.label"
            class="mt-1! size-4! shrink-0 p-0! accent-accent"
            @change="custom[question.id]![field.id] = ''"
          >
          <span class="min-w-0 break-words"><span class="block text-sm font-medium">{{ option.label }}</span><span v-if="option.description" class="mt-1 block text-xs leading-relaxed text-muted">{{ option.description }}</span></span>
        </label>
        <label class="block text-xs text-muted">
          {{ field.options.length ? 'Or write your own answer' : 'Your answer' }}
          <input
            v-if="field.secret"
            v-model="custom[question.id]![field.id]"
            type="password"
            autocomplete="off"
            class="mt-2!"
            maxlength="10000"
            @input="answers[question.id]![field.id] = ''"
          >
          <textarea
            v-else
            v-model="custom[question.id]![field.id]"
            rows="2"
            maxlength="10000"
            class="mt-2! max-h-40 resize-y"
            placeholder="What do you have in mind?"
            @input="answers[question.id]![field.id] = ''"
          />
        </label>
      </fieldset>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
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
