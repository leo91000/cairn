<script setup lang="ts">
import type { AgentPlan } from '../agent-plan'
import {
  computed,
  onBeforeUnmount,
  ref,
  useId,
  watch,
} from 'vue'
import { currentStep, planProgress } from '../agent-plan'
import { ChevronUp, ListTodo } from '../icons'
import Icon from './Icon.vue'
import PlanProgress from './PlanProgress.vue'
import PlanSteps from './PlanSteps.vue'

// « Cap »: the plan of the current request beside the composer, so where the agent stands stays in
// sight while the conversation scrolls. It opens the whole plan above itself.
const props = defineProps<{ plan: AgentPlan }>()
const open = ref(false)
const root = ref<HTMLElement>()
const button = ref<HTMLButtonElement>()
const listId = useId()
const progress = computed(() => planProgress(props.plan.steps))
const current = computed(() => currentStep(props.plan.steps))
const label = computed(() => {
  const step = current.value
  if (!step)
    return ''
  return step.active ? step.step.activeText || step.step.text : `Next: ${step.step.text}`
})
const percent = computed(() => progress.value.total ? Math.round(progress.value.done / progress.value.total * 100) : 0)

function outside(event: PointerEvent) {
  if (!root.value?.contains(event.target as Node))
    open.value = false
}

function escape(event: KeyboardEvent) {
  if (event.key !== 'Escape')
    return
  open.value = false
  button.value?.focus()
}

watch(open, (value) => {
  document.removeEventListener('pointerdown', outside)
  document.removeEventListener('keydown', escape)
  if (value) {
    document.addEventListener('pointerdown', outside)
    document.addEventListener('keydown', escape)
  }
})
onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', outside)
  document.removeEventListener('keydown', escape)
})
</script>

<template>
  <div ref="root" class="plan-bar relative mb-2">
    <section
      v-if="open"
      :id="listId"
      class="absolute inset-x-0 bottom-full z-20 mb-2 max-h-[min(60vh,420px)] overflow-auto rounded-[18px] border border-line bg-surface px-4 pt-3.5 pb-2 shadow-(--shadow-pop)"
      aria-label="Plan"
      data-testid="plan-popover"
    >
      <header class="mb-1 flex items-center gap-2.5">
        <Icon :name="ListTodo" :size="16" class="text-muted" />
        <h3 class="m-0! flex-1 font-heading text-sm font-semibold">
          Plan
        </h3>
        <span class="text-xs text-muted">{{ progress.done }} of {{ progress.total }} done</span>
      </header>
      <p v-if="plan.explanation" class="my-1.5! rounded-[10px] bg-inset px-2.5 py-2 text-xs text-muted">
        {{ plan.explanation }}
      </p>
      <PlanSteps :steps="plan.steps" />
    </section>
    <button
      ref="button"
      class="flex min-h-11 w-full min-w-0 items-center gap-2.5 rounded-2xl border border-line bg-surface px-2.5 py-1.5 text-left hover:bg-hover/60"
      :aria-expanded="open"
      :aria-controls="listId"
      :aria-label="`Plan, ${progress.done} of ${progress.total} done${label ? `. ${label}` : ''}. ${open ? 'Hide' : 'Show'} the plan`"
      data-testid="plan-bar"
      @click="open = !open"
    >
      <span
        class="grid size-5.5 shrink-0 place-items-center rounded-full"
        :style="{ background: `conic-gradient(var(--color-success) ${percent}%, var(--color-variant) 0)` }"
        aria-hidden="true"
      ><span class="size-3.5 rounded-full bg-surface" /></span>
      <span class="shrink-0 font-heading text-xs font-semibold text-ink phone:hidden" aria-hidden="true">Plan</span>
      <PlanProgress :steps="plan.steps" class="w-24 shrink-0 phone:hidden" />
      <span class="min-w-0 flex-1 truncate text-xs text-ink" aria-hidden="true">{{ label }}</span>
      <span class="shrink-0 text-2xs tabular-nums text-muted" aria-hidden="true">{{ progress.done }}/{{ progress.total }}</span>
      <Icon
        :name="ChevronUp"
        :size="16"
        class="shrink-0 text-muted transition-transform"
        :class="open ? 'rotate-180' : ''"
      />
    </button>
  </div>
</template>
