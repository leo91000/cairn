<script setup lang="ts">
import type { WorkingStep } from '../signal'
import { onBeforeUnmount, ref } from 'vue'
import { Zap } from '../icons'
import { liveElapsed } from '../signal'
import Icon from './Icon.vue'

// « Souffle »: a breathing halo, a light sweeping across the current step, its command and the
// elapsed time to the second. Announced politely; the motion stops with reduced motion.
defineProps<{ step: WorkingStep }>()
const now = ref(Date.now())
const timer = setInterval(() => now.value = Date.now(), 1000)
onBeforeUnmount(() => clearInterval(timer))
</script>

<template>
  <div
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
