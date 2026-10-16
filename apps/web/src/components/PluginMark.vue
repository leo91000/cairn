<script setup lang="ts">
import { computed } from 'vue'
import { Puzzle } from '../icons'
import { brands } from '../plugins'
import Icon from './Icon.vue'

// A package's own brand when its manifest declares one, then known brands, then a puzzle.
const props = withDefaults(defineProps<{ plugin: string, size?: number, brand?: { color: string, path?: string } }>(), { size: 44 })
const color = computed(() => props.brand?.color ?? brands[props.plugin]?.color)
</script>

<template>
  <span
    class="grid shrink-0 place-items-center shadow-[inset_0_0_0_1px_#0000000f]"
    :class="color ? 'text-white' : 'bg-soft text-accent'"
    :style="{ width: `${size}px`, height: `${size}px`, borderRadius: `${Math.round(size * 0.29)}px`, background: color }"
    aria-hidden="true"
  >
    <svg
      v-if="brand?.path"
      viewBox="0 0 24 24"
      :width="Math.round(size * 0.52)"
      :height="Math.round(size * 0.52)"
      fill="currentColor"
    ><path :d="brand.path" /></svg>
    <Icon v-else :name="brands[plugin]?.icon ?? Puzzle" :size="Math.round(size * 0.52)" />
  </span>
</template>
