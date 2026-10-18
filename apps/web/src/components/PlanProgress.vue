<script setup lang="ts">
import type { PlanStep } from '../agent-plan'

// One segment per step: done, in progress, dropped or still to do.
defineProps<{ steps: PlanStep[] }>()
const colors = {
  done: 'bg-success',
  active: 'subagent-pulse bg-accent',
  cancelled: 'bg-variant opacity-50',
  pending: 'bg-variant',
}
</script>

<template>
  <span class="flex gap-0.75" aria-hidden="true">
    <span
      v-for="(step, index) in steps"
      :key="index"
      class="h-1.25 min-w-1.5 flex-1 rounded-full"
      :class="colors[step.state]"
    />
  </span>
</template>
