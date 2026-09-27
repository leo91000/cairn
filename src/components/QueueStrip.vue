<script setup lang="ts">
import type { ChatMessage } from '../../shared/chats'
import { computed, ref } from 'vue'
import { Clock, Pause, Pencil, Play, Trash2, Zap } from '../icons'
import Icon from './Icon.vue'
import Modal from './Modal.vue'

// Follow-ups waiting for the agent, drawn at the top of the composer as on Android. The next
// message can be sent now; any message opens its actions (send now, edit, pause, remove).
const props = defineProps<{ pending: ChatMessage[], paused: boolean, working: boolean, busy: boolean }>()
const emit = defineEmits<{ steer: [message: ChatMessage], edit: [message: ChatMessage], remove: [message: ChatMessage], togglePause: [] }>()
const expanded = ref(false)
const selected = ref<string | null>(null)
const headline = computed(() => props.paused ? 'Queue paused · nothing is sent' : props.working ? 'Sent when the agent finishes' : 'Waiting to send')
const chosen = computed(() => props.pending.find(message => message.id === selected.value))
const steerable = (message: ChatMessage) => props.working && message.status === 'queued' && !message.questionId && message.mode !== 'steer'
const preview = (message: ChatMessage) => message.text || message.attachments?.map(file => file.name).join(', ') || 'Attachments'
function note(message: ChatMessage) {
  if (message.status !== 'queued')
    return props.paused ? 'Waiting to resume' : 'Sending…'
  if (message.mode === 'steer')
    return 'Delivered as soon as possible'
  const files = message.attachments?.length ?? 0
  return files ? `${files} attachment${files > 1 ? 's' : ''}` : ''
}
// Close the sheet, then act on the message it showed.
function then(action: (message: ChatMessage) => void) {
  const message = chosen.value
  selected.value = null
  if (message)
    action(message)
}
</script>

<template>
  <div class="queue-strip px-3 pt-2" :class="pending.length > 1 ? '' : 'pb-1'" data-testid="conversation-queue">
    <div class="flex min-h-6 items-center gap-1.5 text-2xs text-muted">
      <Icon :name="Clock" :size="14" />
      <span class="min-w-0 flex-1 truncate" role="status">{{ headline }}{{ pending.length > 1 ? ` · ${pending.length}` : '' }}</span>
      <button v-if="paused" type="button" class="inline-flex min-h-8 items-center gap-1 rounded-md px-2 font-semibold text-accent hover:bg-hover" :disabled="busy" @click="emit('togglePause')">
        <Icon :name="Play" :size="13" />Resume
      </button>
    </div>
    <ul class="m-0! list-none p-0!" :class="expanded ? 'max-h-65 overflow-auto' : ''">
      <li v-for="(message, index) in expanded ? pending : pending.slice(0, 1)" :key="message.id" class="flex items-center gap-2" data-testid="queued-message">
        <button type="button" class="min-w-0 flex-1 rounded-xl py-1.5 text-left hover:bg-hover/50" :aria-label="`Queued message options: ${preview(message)}`" @click="selected = message.id">
          <span class="line-clamp-2 text-base text-ink wrap-anywhere">{{ preview(message) }}</span>
          <span v-if="note(message)" class="block text-2xs text-muted">{{ note(message) }}</span>
        </button>
        <button v-if="index === 0 && steerable(message)" type="button" class="inline-flex h-9 shrink-0 items-center gap-1 rounded-full bg-soft px-3 text-sm font-medium text-accent hover:bg-accent/15 disabled:opacity-50" :disabled="busy" title="The agent reads it without waiting for the end of its task" @click="emit('steer', message)">
          <Icon :name="Zap" :size="13" />Now
        </button>
      </li>
    </ul>
    <button v-if="pending.length > 1" type="button" class="min-h-9 text-xs font-medium text-accent" :aria-expanded="expanded" @click="expanded = !expanded">
      {{ expanded ? 'Show less' : `+ ${pending.length - 1} other message${pending.length > 2 ? 's' : ''}` }}
    </button>
  </div>
  <Modal v-if="chosen" title="Queued message" sheet @close="selected = null">
    <div class="px-5 pb-5" data-testid="queued-message-sheet">
      <p class="mt-3! mb-2! text-xs text-muted">
        {{ headline }}
      </p>
      <p class="mb-2! max-h-55 overflow-auto rounded-[14px] bg-variant p-3 text-base whitespace-pre-wrap wrap-anywhere">
        {{ preview(chosen) }}
      </p>
      <button v-if="steerable(chosen)" type="button" class="queue-action text-accent" :disabled="busy" @click="then(message => emit('steer', message))">
        <Icon :name="Zap" :size="20" /><span><strong>Send now</strong><small>The agent reads it without waiting for the end of its task</small></span>
      </button>
      <button v-if="chosen.status === 'queued' && !chosen.questionId" type="button" class="queue-action" :disabled="busy" @click="then(message => emit('edit', message))">
        <Icon :name="Pencil" :size="20" /><span><strong>Edit</strong></span>
      </button>
      <button type="button" class="queue-action" :disabled="busy" @click="then(() => emit('togglePause'))">
        <Icon :name="paused ? Play : Pause" :size="20" /><span><strong>{{ paused ? 'Resume the queue' : 'Pause the queue' }}</strong><small>{{ paused ? 'Messages go out again in order' : 'Nothing is sent until you resume' }}</small></span>
      </button>
      <button v-if="chosen.status === 'queued'" type="button" class="queue-action text-danger" :disabled="busy" @click="then(message => emit('remove', message))">
        <Icon :name="Trash2" :size="20" /><span><strong>Remove from queue</strong></span>
      </button>
    </div>
  </Modal>
</template>

<style scoped>
.queue-action {
  display: flex;
  width: 100%;
  min-height: 56px;
  align-items: center;
  gap: 16px;
  border-radius: 12px;
  padding: 8px 4px;
  text-align: left;
}
.queue-action:hover:not(:disabled) {
  background: var(--color-hover);
}
.queue-action:disabled {
  opacity: 0.5;
}
.queue-action strong {
  display: block;
  font-size: 16px;
  font-weight: 500;
}
.queue-action small {
  display: block;
  font-size: 12px;
  color: var(--color-muted);
}
</style>
