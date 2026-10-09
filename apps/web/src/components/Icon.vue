<script setup lang="ts">
import type { IconName } from '../icons'

withDefaults(defineProps<{ name: IconName, size?: number }>(), { size: 18 })
</script>

<template>
  <i
    aria-hidden="true"
    class="ui-icon inline-block size-(--icon-size) shrink-0 align-middle not-italic"
    :class="name"
    :style="{ '--icon-size': `${size}px` }"
  />
</template>
