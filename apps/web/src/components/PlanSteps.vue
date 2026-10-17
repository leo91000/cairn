<script setup lang="ts">
import type { PlanStep, PlanStepState } from '../agent-plan'
import { Check, X } from '../icons'
import Icon from './Icon.vue'

// The steps of the agent's plan, with the one in progress and what the agent does during it.
defineProps<{ steps: PlanStep[] }>()
const labels: Record<PlanStepState, string> = {
  pending: 'To do',
  active: 'In progress',
  done: 'Done',
  cancelled: 'Dropped',
}
</script>

<template>
  <ol class="m-0! list-none p-0">
    <li
      v-for="(step, index) in steps"
      :key="`${index}:${step.text}`"
      class="flex items-start gap-2.5 py-1.5"
      :data-state="step.state"
      data-testid="plan-step"
    >
      <span v-if="step.state === 'done'" class="mt-0.5 grid size-4.5 shrink-0 place-items-center rounded-full bg-success text-surface" aria-hidden="true"><Icon :name="Check" :size="11" /></span>
      <span v-else-if="step.state === 'active'" class="mt-0.5 grid size-4.5 shrink-0 place-items-center rounded-full border-2 border-accent" aria-hidden="true"><span class="subagent-pulse size-2 rounded-full bg-accent" /></span>
      <span v-else-if="step.state === 'cancelled'" class="mt-0.5 grid size-4.5 shrink-0 place-items-center rounded-full bg-variant text-subtle" aria-hidden="true"><Icon :name="X" :size="10" /></span>
      <span v-else class="mt-0.5 size-4.5 shrink-0 rounded-full border-[1.5px] border-dashed border-control" aria-hidden="true" />
      <span class="min-w-0 flex-1">
        <span
          class="block text-sm wrap-anywhere"
          :class="{ 'text-muted': step.state === 'done', 'text-subtle line-through': step.state === 'cancelled', 'font-semibold text-ink': step.state === 'active', 'text-ink': step.state === 'pending' }"
        >{{ step.text }}</span>
        <span v-if="step.state === 'active' && step.activeText && step.activeText !== step.text" class="block text-2xs text-muted">{{ step.activeText }}</span>
      </span>
      <span class="sr-only">{{ labels[step.state] }}</span>
    </li>
  </ol>
</template>
