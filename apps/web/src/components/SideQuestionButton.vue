<script setup lang="ts">
import type { SideQuestionIndicator } from '../side-questions'
import { computed } from 'vue'
import { MessageSquareQuote } from '../icons'
import Icon from './Icon.vue'

const props = defineProps<{
  indicator: SideQuestionIndicator
  /** Shown in the conversation header when the working row is absent. */
  header?: boolean
}>()
const emit = defineEmits<{ click: [] }>()

const surfaceClass = computed(() => {
  switch (props.indicator.kind) {
    case 'pending':
      return 'border-accent/30 bg-soft text-accent hover:bg-accent/15'
    case 'ready':
      return 'border-accent/65 bg-soft text-accent hover:bg-accent/15'
    case 'failed':
      return 'border-danger/55 bg-danger/12 text-danger hover:bg-danger/18'
    case 'saved':
      return 'border-line bg-surface text-ink hover:bg-hover'
    default:
      return 'border-line bg-surface text-muted hover:bg-hover hover:text-ink'
  }
})

const badgeClass = computed(() => {
  switch (props.indicator.badgeTone) {
    case 'accent':
      return 'bg-accent text-surface'
    case 'danger':
      return 'bg-danger text-surface'
    default:
      return 'border border-line bg-raised text-ink'
  }
})
</script>

<template>
  <button
    type="button"
    data-testid="side-question-button"
    class="relative inline-grid shrink-0 place-items-center rounded-full border transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
    :class="[
      header ? 'size-9 pointer-coarse:size-10' : 'size-9 pointer-coarse:size-10',
      surfaceClass,
    ]"
    :aria-label="indicator.label"
    :title="indicator.label"
    @click="emit('click')"
  >
    <svg
      v-if="indicator.kind === 'pending'"
      aria-hidden="true"
      viewBox="0 0 36 36"
      class="side-question-spin pointer-events-none absolute -inset-px size-[calc(100%+2px)]"
    >
      <circle
        cx="18"
        cy="18"
        r="17"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-dasharray="28 79"
        transform="rotate(-90 18 18)"
      />
    </svg>
    <Icon :name="MessageSquareQuote" :size="header ? 17 : 16" />
    <span
      v-if="indicator.badge"
      aria-hidden="true"
      class="pointer-events-none absolute -top-1 -right-1 grid h-4 min-w-4 place-items-center rounded-full px-1 text-[10px] leading-none font-bold ring-2 ring-canvas"
      :class="badgeClass"
    >{{ indicator.badge }}</span>
  </button>
</template>
