<script setup lang="ts">
import type { ChipTone } from '../agent-access'
import { Lock } from '../icons'
import Icon from './Icon.vue'

// Access states always carry text; colour only reinforces it.

defineProps<{ tone: ChipTone }>()
const tones: Record<ChipTone, string> = {
  granted: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  none: 'bg-hover text-muted',
  unavailable: 'bg-warning-surface text-warning',
  missing: 'bg-danger-surface text-danger',
  fixed: 'bg-hover text-ink',
  power: 'bg-power-surface text-power',
}
</script>

<template>
  <span class="inline-flex min-h-5.5 max-w-full shrink-0 items-center gap-1 rounded-full px-2.25 py-0.5 text-2xs font-semibold leading-tight" :class="tones[tone]">
    <Icon v-if="tone === 'fixed'" :name="Lock" :size="11" />
    <span class="min-w-0 truncate"><slot /></span>
  </span>
</template>
