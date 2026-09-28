<script setup lang="ts">
import { identityColor } from '../signal'

// A project's identity dot and name. No project means every allowed project: a neutral outline.
defineProps<{ name: string, identity?: string | null }>()
</script>

<template>
  <span class="flex min-w-0 items-center gap-1.5 text-xs text-muted">
    <span class="size-1.75 shrink-0 rounded-full" :class="identity ? '' : 'border border-muted'" :style="identity ? { background: identityColor(identity) } : undefined" />
    <span class="truncate">{{ name }}</span>
  </span>
</template>
