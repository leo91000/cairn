<script setup lang="ts">
import { computed } from 'vue'
import { lowPercent } from '../../shared/accounts'

// Remaining usage as a thin bar: low turns amber, empty coral, a paused account grey.
const props = defineProps<{ remaining: number, muted?: boolean, label: string }>()
const tone = computed(() => props.muted ? 'bg-subtle opacity-60' : props.remaining <= 0 ? 'bg-coral' : props.remaining < lowPercent ? 'bg-warning' : 'bg-accent')
</script>

<template>
  <span
    role="progressbar"
    :aria-label="label"
    :aria-valuenow="Math.round(remaining)"
    aria-valuemin="0"
    aria-valuemax="100"
    class="block h-[5px] overflow-hidden rounded-full bg-hover"
  >
    <span class="block h-full rounded-full transition-[width] duration-500 motion-reduce:transition-none" :class="tone" :style="{ width: `${Math.max(remaining, remaining > 0 ? 2 : 0)}%` }" />
  </span>
</template>
