<script setup lang="ts">
import type { RunStatus, TaskOutcome } from '../../../../packages/contracts/contracts'

defineProps<{ outcome?: TaskOutcome | null, status: RunStatus }>()
const labels = { completed: 'Completed', blocked: 'Blocked', needs_input: 'Your input needed' }
</script>

<template>
  <details v-if="outcome && status === 'succeeded'" class="max-w-full text-xs" :class="outcome.status === 'completed' ? 'text-muted' : 'text-warning'">
    <summary class="cursor-pointer font-medium">
      {{ labels[outcome.status] }}
    </summary>
    <div class="mt-2 max-w-prose space-y-2 text-foreground">
      <p>{{ outcome.reason }}</p>
      <ul v-if="outcome.evidence.length" class="list-inside list-disc break-words">
        <li v-for="(entry, index) in outcome.evidence" :key="index">
          {{ entry }}
        </li>
      </ul>
      <p class="text-[10px] text-muted">
        Reported by the agent
      </p>
    </div>
  </details>
</template>
