<script setup lang="ts">
import type { ButtonSize, ButtonVariant } from '../ui'
import { twMerge } from 'tailwind-merge'
import { buttonBase, buttonSizes, buttonVariants } from '../ui'

defineOptions({ inheritAttrs: false })
const props = withDefaults(defineProps<{
  class?: string
  variant?: ButtonVariant
  size?: ButtonSize
  type?: 'button' | 'submit' | 'reset'
}>(), { variant: 'default', size: 'default', type: 'button' })
</script>

<template>
  <button v-bind="$attrs" :type="type" :class="twMerge(buttonBase, buttonVariants[variant], buttonSizes[size], props.class)">
    <slot />
  </button>
</template>
