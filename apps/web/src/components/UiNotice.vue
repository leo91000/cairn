<script setup lang="ts">
import type { IconName } from '../icons'
import { computed } from 'vue'
import { CircleAlert, Clock, TriangleAlert } from '../icons'
import Icon from './Icon.vue'

// One way to state something beside the work: a tinted icon carries the tone, the text stays
// readable on a quiet card.
const props = withDefaults(defineProps<{
  tone?: 'info' | 'warning' | 'danger'
  title?: string
  icon?: IconName
}>(), { tone: 'info', title: undefined, icon: undefined })
const icon = computed(() => props.icon ?? (props.tone === 'danger' ? CircleAlert : props.tone === 'warning' ? TriangleAlert : Clock))
const tint = computed(() => props.tone === 'danger' ? 'bg-danger-surface text-danger' : props.tone === 'warning' ? 'bg-warning-surface text-warning' : 'bg-soft text-accent')
</script>

<template>
  <div class="ui-notice my-3 flex min-w-0 items-start gap-2.5 rounded-lg border bg-surface px-3 py-2.5 text-sm leading-relaxed text-ink" :class="tone === 'danger' ? 'border-danger/30' : 'border-line'">
    <span class="grid size-6 shrink-0 place-items-center rounded-md" :class="tint">
      <Icon :name="icon" :size="14" />
    </span>
    <div class="min-w-0 flex-1 self-center wrap-anywhere">
      <p v-if="title" class="font-semibold">
        {{ title }}
      </p>
      <div v-if="$slots.default" :class="title ? 'text-muted' : ''">
        <slot />
      </div>
    </div>
    <slot name="action" />
  </div>
</template>
