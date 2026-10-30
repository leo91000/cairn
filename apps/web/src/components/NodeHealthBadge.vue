<script setup lang="ts">
import type { NodeHealth, NodeHealthState } from '../../../../packages/contracts/nodes'

// A node's health as a small pill: green when it takes work, amber when it waits, coral when it needs you.
defineProps<{ health: NodeHealth }>()
const tones: Record<NodeHealthState, string> = {
  ready: 'bg-success/12 text-success',
  full: 'bg-warning-surface text-warning',
  busy: 'bg-warning-surface text-warning',
  updating: 'bg-warning-surface text-warning',
  setup: 'bg-warning-surface text-warning',
  reserved: 'bg-power-surface text-power',
  paused: 'bg-hover text-muted',
  revoked: 'bg-hover text-muted',
  attention: 'bg-coral-soft text-coral',
  offline: 'bg-coral-soft text-coral',
}
</script>

<template>
  <span class="node-health inline-flex h-[22px] shrink-0 items-center gap-[5px] rounded-full px-2.5 text-[11.5px] font-semibold whitespace-nowrap" :class="tones[health.state]" :data-state="health.state">
    <span class="size-1.5 rounded-full bg-current" />{{ health.label }}
  </span>
</template>
