<script setup lang="ts">
import { Puzzle } from '../icons'
import { brands } from '../plugins'
import Icon from './Icon.vue'

withDefaults(defineProps<{ plugin: string, size?: number }>(), { size: 44 })
</script>

<template>
  <span
    class="grid shrink-0 place-items-center shadow-[inset_0_0_0_1px_#0000000f]"
    :class="brands[plugin] ? 'text-white' : 'bg-soft text-accent'"
    :style="{ width: `${size}px`, height: `${size}px`, borderRadius: `${Math.round(size * 0.29)}px`, background: brands[plugin]?.color }"
    aria-hidden="true"
  >
    <Icon :name="brands[plugin]?.icon ?? Puzzle" :size="Math.round(size * 0.52)" />
  </span>
</template>
