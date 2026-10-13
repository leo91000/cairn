<script setup lang="ts">
import type { ActivityArtifact } from '../activity'
import { subagentColor, subagentInitials } from '../subagents'

withDefaults(defineProps<{ agent: ActivityArtifact, size?: number }>(), { size: 28 })
</script>

<template>
  <span
    class="grid shrink-0 place-items-center rounded-[30%] font-bold text-white"
    :style="{ background: subagentColor(agent), width: `${size}px`, height: `${size}px`, fontSize: `${Math.round(size * 0.375)}px` }"
    aria-hidden="true"
  >{{ subagentInitials(agent.title) }}</span>
</template>
