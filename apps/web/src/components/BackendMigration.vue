<script setup lang="ts">
import { onUnmounted, ref } from 'vue'
import { api } from '../api'

/** A conversation's backend and, for an environment created natively, its one-time conversion for a VM. */
export interface BackendMigrationState {
  chatId: string
  backend: 'native' | 'firecracker'
  diskFormat: 'vm' | 'native'
  nodes: {
    id: string
    name: string
    backend: 'native' | 'firecracker'
    conversion: boolean
    nativeAuthorityConfirmed: boolean
  }[]
  pending: boolean
  error?: string | null
}

const props = defineProps<{ runId: string }>()
const emit = defineEmits<{ completed: [] }>()
const state = ref<BackendMigrationState>()
const completed = ref(false)
let stopped = false
let timer: ReturnType<typeof setTimeout> | undefined
onUnmounted(() => {
  stopped = true
  clearTimeout(timer)
})

async function load() {
  try {
    const value = await api<BackendMigrationState>(`/nodes/placement/${props.runId}/backend-migration`)
    if (stopped)
      return
    if (state.value?.pending && !value.pending && !value.error) {
      completed.value = true
      emit('completed')
    }

    state.value = value
  }
  catch {
    // Older installations and non-owner views have no conversion progress to show.
  }

  if (!stopped && state.value?.pending)
    timer = setTimeout(load, 1500)
}

void load()
</script>

<template>
  <section v-if="state?.pending || state?.error || completed" class="mt-3 border-t border-line pt-3" aria-label="Environment conversion">
    <h3 class="m-0! text-xs! font-semibold text-ink">
      Moving to a VM
    </h3>
    <p v-if="completed" role="status">
      Conversion complete. Send a new message to continue in the same conversation.
    </p>
    <p v-else-if="state?.pending" role="status">
      Converting this natively created environment for a VM. Execution is stopped while its files are copied and restored. You can leave this screen.
    </p>
    <p v-if="state?.error" role="alert" class="text-coral">
      {{ state.error }} Move the conversation again to retry.
    </p>
  </section>
</template>
