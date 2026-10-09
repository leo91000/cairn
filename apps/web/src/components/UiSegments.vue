<script setup lang="ts" generic="T extends string | boolean">
import type { IconName } from '../icons'
import Icon from './Icon.vue'

defineProps<{
  label: string
  options: {
    value: T
    label: string
    icon?: IconName
    count?: number
  }[]
  compact?: boolean
}>()
const model = defineModel<T>({ required: true })
</script>

<template>
  <div
    class="tabs flex min-w-0 max-w-full items-center gap-1"
    :class="compact ? 'flex-wrap phone:gap-0' : 'w-full'"
    role="group"
    :aria-label="label"
  >
    <button
      v-for="option in options"
      :key="`${option.value}`"
      type="button"
      class="flex min-h-[38px] items-center justify-center gap-[7px] rounded-md px-3 py-[9px] text-2xs font-medium whitespace-nowrap transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      :class="[model === option.value ? 'selected bg-hover/60 text-ink' : 'text-muted hover:bg-soft hover:text-ink', compact ? 'phone:min-h-[34px] phone:px-2 phone:text-3xs' : 'phone:flex-1 phone:gap-[5px] phone:px-2.5']"
      :aria-pressed="model === option.value"
      @click="model = option.value"
    >
      <Icon v-if="option.icon" :name="option.icon" :size="15" />
      {{ option.label }}
      <span v-if="option.count !== undefined" class="min-w-6 rounded bg-transparent px-[5px] py-0.5 text-center text-micro tabular-nums phone:min-w-5 phone:px-[3px]">{{ option.count }}</span>
    </button>
  </div>
</template>
