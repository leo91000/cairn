<script setup lang="ts">
import type { AgentPlan } from '../agent-plan'
import { computed, ref, useId } from 'vue'
import { planFinished, planProgress } from '../agent-plan'
import { Check, ChevronDown, ListTodo } from '../icons'
import { liveElapsed } from '../signal'
import Icon from './Icon.vue'
import PlanProgress from './PlanProgress.vue'
import PlanSteps from './PlanSteps.vue'

// The agent's plan where it first wrote it, kept up to date by its later updates. A finished
// plan folds into its summary.
const props = defineProps<{ plan: AgentPlan, live: boolean }>()
const listId = useId()
const toggled = ref<boolean | null>(null)
const finished = computed(() => planFinished(props.plan.steps))
const progress = computed(() => planProgress(props.plan.steps))
const open = computed(() => toggled.value ?? !finished.value)
// A finished plan says how long the agent took to follow it.
const summary = computed(() => {
  const steps = progress.value.total === 1 ? '1 step' : `${progress.value.total} steps`
  if (!finished.value)
    return `${steps} · ${progress.value.done} done`
  const took = props.plan.updatedAt - props.plan.time >= 1000 ? liveElapsed(props.plan.time, props.plan.updatedAt) : ''
  return [steps, took].filter(Boolean).join(' · ')
})
</script>

<template>
  <section class="agent-plan my-3 overflow-hidden rounded-[18px] border border-line bg-surface" aria-label="Plan" data-testid="agent-plan">
    <button
      class="flex min-h-12 w-full items-center gap-2.5 px-3 py-2.5 text-left"
      :aria-expanded="open"
      :aria-controls="listId"
      @click="toggled = !open"
    >
      <span class="grid size-7.5 shrink-0 place-items-center rounded-[10px]" :class="finished ? 'bg-variant text-success' : 'bg-soft text-accent'" aria-hidden="true">
        <Icon :name="finished ? Check : ListTodo" :size="15" />
      </span>
      <span class="min-w-0 flex-1">
        <span class="block font-heading text-base font-semibold text-ink">{{ finished ? 'Plan complete' : 'Plan' }}</span>
        <span class="block text-2xs text-muted">{{ summary }}</span>
      </span>
      <PlanProgress v-if="!finished" :steps="plan.steps" class="w-28 shrink-0 phone:hidden" />
      <span v-if="live" class="shrink-0 rounded-full bg-soft px-2 py-0.5 text-3xs font-bold tracking-wider text-accent uppercase">Live</span>
      <Icon
        :name="ChevronDown"
        :size="18"
        class="shrink-0 text-muted transition-transform"
        :class="open ? 'rotate-180' : ''"
      />
    </button>
    <div v-if="open" :id="listId" class="px-3.5 pb-2">
      <p v-if="plan.explanation" class="mb-1! rounded-[10px] bg-inset px-2.5 py-2 text-xs text-muted">
        {{ plan.explanation }}
      </p>
      <PlanSteps :steps="plan.steps" />
    </div>
  </section>
</template>
