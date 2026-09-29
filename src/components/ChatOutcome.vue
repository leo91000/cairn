<script setup lang="ts">
import type { TaskOutcome } from '../../shared/contracts'
import { computed, ref, useId } from 'vue'
import {
  Check,
  ChevronDown,
  CircleAlert,
  FileText,
  Info,
} from '../icons'
import Icon from './Icon.vue'
import Markdown from './Markdown.vue'

const props = defineProps<{ outcome: TaskOutcome, agent: string }>()
const expanded = ref(false)
const evidenceId = useId()
const attention = computed(() => props.outcome.status !== 'completed')
const labels = { completed: 'Task completed', blocked: 'Blocked', needs_input: 'Your input needed' }
</script>

<template>
  <section class="chat-outcome mt-4 min-w-0 text-xs" aria-label="Task outcome" :class="attention ? 'rounded-xl border border-warning/30 bg-warning-surface p-4' : ''">
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1">
      <span class="inline-flex items-center gap-1.5 font-medium" :class="attention ? 'text-warning' : 'text-accent'">
        <Icon :name="attention ? CircleAlert : Check" :size="14" />{{ labels[outcome.status] }}
      </span>
      <span class="text-subtle" aria-hidden="true">·</span>
      <button
        class="inline-flex min-h-8 items-center gap-1.5 rounded-sm text-muted hover:text-ink phone:min-h-11"
        :aria-expanded="expanded"
        :aria-controls="evidenceId"
        @click="expanded = !expanded"
      >
        {{ expanded ? 'Hide' : 'View' }} {{ outcome.evidence.length ? 'evidence' : 'details' }}
        <Icon :name="ChevronDown" :size="13" :class="expanded ? 'rotate-180' : ''" />
      </button>
    </div>
    <div v-if="attention" class="outcome-content mt-2 text-ink">
      <Markdown :content="outcome.reason" />
    </div>
    <div
      v-if="expanded"
      :id="evidenceId"
      class="mt-3 max-w-prose border-l-2 pl-4"
      :class="attention ? 'border-warning/40' : 'border-accent/40'"
    >
      <div v-if="!attention" class="outcome-content mb-3 text-muted">
        <Markdown :content="outcome.reason" />
      </div>
      <ul v-if="outcome.evidence.length" class="divide-y divide-line/70">
        <li v-for="(entry, index) in outcome.evidence" :key="index" class="flex min-w-0 items-start gap-3 py-3 first:pt-0">
          <Icon :name="FileText" :size="15" class="mt-0.5 shrink-0 text-subtle" />
          <div class="outcome-content min-w-0 flex-1 text-ink">
            <Markdown :content="entry" compact-links />
          </div>
        </li>
      </ul>
      <p class="mt-3 flex flex-wrap items-center gap-1.5 text-3xs text-muted">
        <Icon :name="Info" :size="12" />Reported by {{ agent }}
        <time :datetime="new Date(outcome.reportedAt).toISOString()" :title="new Date(outcome.reportedAt).toLocaleString()">at {{ new Date(outcome.reportedAt).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) }}</time>
      </p>
    </div>
  </section>
</template>

<style scoped>
.outcome-content :deep(.markdown) {
  font-size: 12px;
  line-height: 1.7;
}
.outcome-content :deep(.markdown > :last-child) {
  margin-bottom: 0;
}
</style>
