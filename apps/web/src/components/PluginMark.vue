<script setup lang="ts">
import { brands } from '../plugins'
import Icon from './Icon.vue'

withDefaults(defineProps<{ plugin: string, size?: number }>(), { size: 44 })
</script>

<template>
  <span
    class="grid shrink-0 place-items-center text-white shadow-[inset_0_0_0_1px_#0000000f]"
    :style="{ width: `${size}px`, height: `${size}px`, borderRadius: `${Math.round(size * 0.29)}px`, background: brands[plugin]?.color }"
    aria-hidden="true"
  >
    <Icon v-if="brands[plugin]" :name="brands[plugin].icon" :size="Math.round(size * 0.52)" />
  </span>
</template>
