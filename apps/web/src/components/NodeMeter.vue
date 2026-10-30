<script setup lang="ts">
import { computed } from 'vue'

// Capacity in use as a thin bar: amber from three quarters, coral when nearly full.
const props = defineProps<{ used: number, label: string }>()
const tone = computed(() => props.used >= 90 ? 'bg-coral' : props.used >= 75 ? 'bg-warning' : 'bg-accent')
</script>

<template>
  <span
    role="progressbar"
    :aria-label="label"
    :aria-valuenow="Math.round(used)"
    aria-valuemin="0"
    aria-valuemax="100"
    class="block h-[5px] overflow-hidden rounded-full bg-hover"
  >
    <span class="block h-full rounded-full transition-[width] duration-500 motion-reduce:transition-none" :class="tone" :style="{ width: `${Math.max(used, used > 0 ? 2 : 0)}%` }" />
  </span>
</template>
