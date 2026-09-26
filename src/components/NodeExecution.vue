<script setup lang="ts">
import type { Run } from '../../shared/contracts'
import type { ExecutionNode } from '../../shared/nodes'
import { computed, onUnmounted, ref, watch } from 'vue'
import { api } from '../api'
import UiButton from './UiButton.vue'

const props = defineProps<{ run: Run }>()
const nodes = ref<ExecutionNode[]>([])
const labels: Record<string, string> = { 'pausing': 'Pausing the VM', 'saving': 'Saving the environment', 'restoring': 'Restoring the environment', 'resuming': 'Resuming the conversation', 'waiting-for-node': 'Waiting for a compatible node', 'updating': 'Updating the node' }
const node = computed(() => nodes.value.find(node => node.id === props.run.nodeId))
const selection = ref('')
const mode = ref('automatic')
const busy = ref(false)
const error = ref('')
const now = ref(Date.now())
const timer = setInterval(() => {
  now.value = Date.now()
}, 1000)
onUnmounted(() => clearInterval(timer))
const age = computed(() => props.run.backup?.capturedAt ? Math.max(0, Math.floor((now.value - props.run.backup.capturedAt) / 1000)) : null)
watch(() => props.run.id, async () => {
  try {
    const value = await api<{ nodes: ExecutionNode[], pinnedNodeId: string | null, preferredNodeId: string | null }>(`/nodes/placement/${props.run.id}`)
    nodes.value = value.nodes
    mode.value = value.pinnedNodeId ? 'fixed' : value.preferredNodeId ? 'preferred' : 'automatic'
    selection.value = value.pinnedNodeId || value.preferredNodeId || props.run.nodeId || ''
  }
  catch (e) { error.value = e instanceof Error ? e.message : 'Unable to load placement' }
}, { immediate: true })
async function save(move = false) {
  busy.value = true
  error.value = ''
  try {
    await api(`/nodes/placement/${props.run.id}`, { method: 'PUT', body: JSON.stringify({ pinnedNodeId: mode.value === 'fixed' ? selection.value : null, preferredNodeId: mode.value === 'preferred' ? selection.value : null }) })
    if (move)
      await api(`/nodes/placement/${props.run.id}/move`, { method: 'POST', body: JSON.stringify({ nodeId: selection.value, ...(props.run.resources || { cpu: 2, memoryMiB: 4096, diskMiB: 32768 }) }) })
  }
  catch (e) { error.value = e instanceof Error ? e.message : 'Unable to update placement' }
  finally { busy.value = false }
}
</script>

<template>
  <div v-if="run.nodeId" class="border-b border-line px-6 py-2 text-xs text-muted" role="status" aria-live="polite">
    <span class="font-semibold">{{ node?.name || run.nodeId }}</span>
    <span v-if="run.resources"> · {{ run.resources.cpu }} CPU · {{ run.resources.memoryMiB }} MiB RAM</span>
    <span v-if="run.pinnedNodeId"> · Fixed node; automatic failover disabled</span>
    <p v-if="run.nodeState" class="mt-1">
      {{ labels[run.nodeState] || run.nodeState }}
    </p>
    <p v-if="run.capacityWaitUntil" class="mt-1">
      Waiting for capacity until {{ new Date(run.capacityWaitUntil).toLocaleString() }}.
    </p>
    <p v-if="run.backup?.capturedAt" class="mt-1">
      Recoverable VM state: {{ new Date(run.backup.capturedAt).toLocaleString() }} ({{ age }} seconds old). Newer chat and file changes may not be in this recovery point.
    </p>
    <p v-if="run.backup?.status === 'saving'" class="mt-1">
      Uploading changed disk blocks…
    </p>
    <p v-if="run.backup?.error" class="mt-1 text-coral">
      Backup: {{ run.backup.error }}
    </p>
    <p v-if="run.restoredAt" class="mt-1">
      Resumed from {{ new Date(run.restoredAt).toLocaleString() }}. More recent messages remain visible; restored files can be older.
    </p>
    <p v-if="run.movementError" class="mt-1 text-coral">
      {{ run.movementError }}
    </p>
    <details class="mt-2">
      <summary>Execution node</summary>
      <div class="mt-2 flex flex-wrap items-center gap-2">
        <label>Placement <select v-model="mode" aria-label="Placement" :disabled="busy" class="rounded border border-line bg-surface p-1"><option value="automatic">Automatic</option><option value="preferred">Prefer a node</option><option value="fixed">Fix to a node</option></select></label>
        <label>Node <select v-model="selection" aria-label="Node" :disabled="busy" class="rounded border border-line bg-surface p-1"><option value="" disabled>Select a node</option><option v-for="candidate in nodes" :key="candidate.id" :value="candidate.id">{{ candidate.name }}</option></select></label>
        <UiButton size="small" :disabled="busy || (mode !== 'automatic' && !selection)" @click="save()">
          Save placement
        </UiButton>
        <UiButton size="small" :disabled="busy || !selection || !['running', 'succeeded'].includes(run.status) || !!run.nodeState" @click="save(true)">
          Move now
        </UiButton>
      </div>
      <p class="mt-1">
        A preference permits failover. A fixed node waits for that machine. Move now pauses the active conversation after reserving its destination.
      </p>
      <p v-if="error" role="alert" class="mt-1 text-coral">
        {{ error }}
      </p>
    </details>
  </div>
</template>
