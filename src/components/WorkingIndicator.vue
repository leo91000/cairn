<script setup lang="ts">
import type { WorkingStep } from '../signal'
import { computed, onBeforeUnmount, ref } from 'vue'
import { Terminal, Zap } from '../icons'
import { liveElapsed } from '../signal'
import Icon from './Icon.vue'

// « Souffle »: a breathing halo, a light sweeping across the current step, its command and the
// elapsed time to the second. Announced politely; the motion stops with reduced motion.
// « Veille »: the agent finished responding and only waits for its background tasks. A slow
// ring replaces the breath so a long wait never looks like an agent thinking for hours.
const props = defineProps<{ step: WorkingStep }>()
const now = ref(Date.now())
const timer = setInterval(() => now.value = Date.now(), 1000)
onBeforeUnmount(() => clearInterval(timer))

const MAX_TASKS = 3
const shownTasks = computed(() => props.step.waiting?.slice(0, MAX_TASKS) ?? [])
const hiddenTasks = computed(() => Math.max(0, (props.step.waiting?.length ?? 0) - MAX_TASKS))
</script>

<template>
  <div
    v-if="step.waiting"
    class="agent-waiting flex min-w-0 items-start gap-3"
    role="status"
    aria-live="polite"
    :aria-label="`${step.title}: ${step.detail}. The agent resumes when it finishes.`"
  >
    <span class="veille-ring size-9 shrink-0">
      <span class="veille-core grid size-5 place-items-center rounded-full">
        <Icon :name="Terminal" :size="10" />
      </span>
    </span>
    <span class="min-w-0 flex-1 pt-0.5" aria-hidden="true">
      <span class="block truncate font-heading text-[14.5px] font-semibold">{{ step.title }}</span>
      <span class="mt-1.5 flex min-w-0 flex-wrap gap-1.5">
        <span
          v-for="(task, index) in shownTasks"
          :key="index"
          class="veille-task inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-full px-2 py-0.5 text-[11.5px]"
          :title="task"
        >
          <span class="veille-dot size-1.5 shrink-0 rounded-full" />
          <span class="truncate">{{ task }}</span>
        </span>
        <span v-if="hiddenTasks" class="veille-task inline-flex items-center rounded-full px-2 py-0.5 text-[11.5px]">+{{ hiddenTasks }}</span>
      </span>
      <span class="mt-1.5 block text-[11.5px] text-muted">The agent resumes when it finishes.</span>
    </span>
    <span v-if="step.since" class="shrink-0 pt-0.5 text-xs font-medium tabular-nums text-muted" aria-hidden="true">{{ liveElapsed(step.since, now) }}</span>
  </div>
  <div
    v-else
    class="agent-working flex min-w-0 items-center gap-3"
    role="status"
    aria-live="polite"
    :aria-label="[step.title, step.detail].filter(Boolean).join(': ')"
  >
    <span class="souffle-halo size-9 shrink-0">
      <span class="souffle-core grid size-5 place-items-center rounded-full bg-accent text-surface">
        <Icon :name="Zap" :size="10" />
      </span>
    </span>
    <span class="min-w-0 flex-1" aria-hidden="true">
      <span class="souffle-sweep block truncate font-heading text-[14.5px] font-semibold">{{ step.title }}…</span>
      <span v-if="step.detail" class="mt-0.5 block truncate text-[11.5px] text-muted" :class="{ 'font-mono': !step.detail.startsWith('Last step') }">{{ step.detail }}</span>
    </span>
    <span v-if="step.since" class="shrink-0 text-xs font-medium tabular-nums text-muted" aria-hidden="true">{{ liveElapsed(step.since, now) }}</span>
  </div>
</template>
