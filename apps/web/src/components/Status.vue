<script setup lang="ts">
defineProps<{ status: string }>()
const tones: Record<string, string> = {
  succeeded: 'bg-soft text-accent',
  running: 'bg-warning-surface text-warning',
  queued: 'bg-warning-surface text-warning',
  failed: 'bg-danger-surface text-danger',
  interrupted: 'bg-danger-surface text-danger',
}
</script>

<template>
  <span class="status inline-flex items-center gap-[5px] rounded-[5px] px-[7px] py-1 text-xs whitespace-nowrap capitalize" :class="[status, tones[status] ?? 'bg-soft text-muted']"><span class="size-[5px] rounded-full bg-current" />{{ status === 'succeeded' ? 'Execution finished' : status }}</span>
</template>
