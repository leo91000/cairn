<script setup lang="ts">
import type { PlanChange, PlanProgress } from '../agent-plan'
import { planChangeLabel } from '../agent-plan'
import {
  ArrowRight,
  Check,
  Plus,
  X,
} from '../icons'
import Icon from './Icon.vue'

// Where the agent changed its plan: what it finished, started, added or dropped.
defineProps<{ change: PlanChange, progress: PlanProgress }>()
</script>

<template>
  <p class="plan-update my-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-[14px] border border-dashed border-line bg-inset px-3 py-1.5 text-xs" data-testid="plan-update">
    <span class="sr-only">{{ planChangeLabel(change) }}, {{ progress.done }} of {{ progress.total }} done</span>
    <span class="text-3xs font-bold tracking-wider text-muted uppercase" aria-hidden="true">Plan</span>
    <span
      v-for="step in change.done"
      :key="`done:${step}`"
      class="inline-flex min-w-0 max-w-full items-center gap-1 text-success"
      aria-hidden="true"
    ><Icon :name="Check" :size="12" /><span class="truncate">{{ step }}</span></span>
    <span
      v-for="step in change.started"
      :key="`started:${step}`"
      class="inline-flex min-w-0 max-w-full items-center gap-1 font-semibold text-accent"
      aria-hidden="true"
    ><Icon :name="ArrowRight" :size="12" /><span class="truncate">{{ step }}</span></span>
    <span
      v-for="step in change.added"
      :key="`added:${step}`"
      class="inline-flex min-w-0 max-w-full items-center gap-1 text-muted"
      aria-hidden="true"
    ><Icon :name="Plus" :size="12" /><span class="truncate">{{ step }}</span></span>
    <span
      v-for="step in change.cancelled"
      :key="`cancelled:${step}`"
      class="inline-flex min-w-0 max-w-full items-center gap-1 text-subtle"
      aria-hidden="true"
    ><Icon :name="X" :size="12" /><span class="truncate line-through">{{ step }}</span></span>
    <span class="ml-auto text-2xs tabular-nums text-muted" aria-hidden="true">{{ progress.done }}/{{ progress.total }}</span>
  </p>
</template>
