<script setup lang="ts">
import { computed } from 'vue'
import { identityColor } from '../signal'

// External people (Discord users) get initials only: their avatars live on a third-party CDN.
const props = withDefaults(defineProps<{ name: string, identity: string, size?: number }>(), { size: 28 })
const initials = computed(() => props.name.split(/\s+/).filter(Boolean).slice(0, 2).map(part => [...part][0]).join('').toUpperCase() || '?')
</script>

<template>
  <span
    class="inline-grid shrink-0 place-items-center rounded-full font-bold text-white ring-2 ring-surface select-none"
    :style="{ width: `${size}px`, height: `${size}px`, fontSize: `${Math.round(size * 0.38)}px`, background: identityColor(identity) }"
    aria-hidden="true"
  >{{ initials }}</span>
</template>
