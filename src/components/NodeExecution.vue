<script setup lang="ts">
import type { Run } from '../../shared/contracts'
import type { ExecutionNode } from '../../shared/nodes'
import { computed, onUnmounted, ref, watch } from 'vue'
import { DEFAULT_RESOURCES, formatMiB, LOCAL_NODE_ID, relativeAge } from '../../shared/nodes'
import { api } from '../api'
import UiButton from './UiButton.vue'

const props = defineProps<{ run: Run }>()
const nodes = ref<ExecutionNode[]>([])
const labels: Record<string, string> = { 'pausing': 'Pausing the VM', 'saving': 'Saving the environment', 'restoring': 'Restoring the environment', 'resuming': 'Resuming the conversation', 'waiting-for-node': 'Waiting for a compatible node', 'updating': 'Updating the node' }
const node = computed(() => nodes.value.find(node => node.id === props.run.nodeId))
const selection = ref('')
const mode = ref('automatic')
const destination = ref('')
const cpu = ref(DEFAULT_RESOURCES.cpu)
const memoryGiB = ref(DEFAULT_RESOURCES.memoryMiB / 1024)
const diskGiB = ref(DEFAULT_RESOURCES.diskMiB / 1024)
const busy = ref(false)
const error = ref('')
const saved = ref('')
// Recovery ages are shown to the minute, so a slow clock is enough.
const now = ref(Date.now())
const timer = setInterval(() => {
  now.value = Date.now()
}, 30_000)
onUnmounted(() => clearInterval(timer))
// With only the master runner there is nothing to choose, so stay out of the way unless something happens.
const relevant = computed(() => (!!props.run.nodeId && props.run.nodeId !== LOCAL_NODE_ID)
  || nodes.value.some(candidate => !candidate.local)
  || !!(props.run.nodeState || props.run.movementError || props.run.restoredAt || props.run.capacityWaitUntil || props.run.backup?.error))
const destinations = computed(() => nodes.value.filter(candidate => candidate.id !== props.run.nodeId))
const minimumDiskGiB = computed(() => (props.run.resources?.diskMiB ?? 128) / 1024)
const canMove = computed(() => !busy.value && !!destination.value && destination.value !== props.run.nodeId && ['running', 'succeeded'].includes(props.run.status) && !props.run.nodeState)
watch(() => props.run.id, async () => {
  const resources = props.run.resources || DEFAULT_RESOURCES
  cpu.value = resources.cpu
  memoryGiB.value = resources.memoryMiB / 1024
  diskGiB.value = resources.diskMiB / 1024
  try {
    const value = await api<{ nodes: ExecutionNode[], pinnedNodeId: string | null, preferredNodeId: string | null }>(`/nodes/placement/${props.run.id}`)
    nodes.value = value.nodes
    mode.value = value.pinnedNodeId ? 'fixed' : value.preferredNodeId ? 'preferred' : 'automatic'
    selection.value = value.pinnedNodeId || value.preferredNodeId || props.run.nodeId || ''
    destination.value = destinations.value[0]?.id || ''
  }
  catch (e) { error.value = e instanceof Error ? e.message : 'Unable to load placement' }
}, { immediate: true })
async function request(operation: () => Promise<void>) {
  busy.value = true
  error.value = ''
  saved.value = ''
  try {
    await operation()
  }
  catch (e) { error.value = e instanceof Error ? e.message : 'Unable to update placement' }
  finally { busy.value = false }
}
function savePreference() {
  return request(async () => {
    await api(`/nodes/placement/${props.run.id}`, { method: 'PUT', body: JSON.stringify({ pinnedNodeId: mode.value === 'fixed' ? selection.value : null, preferredNodeId: mode.value === 'preferred' ? selection.value : null }) })
    saved.value = 'Preference saved. It applies the next time this conversation starts or recovers.'
  })
}
function move() {
  return request(async () => {
    await api(`/nodes/placement/${props.run.id}/move`, { method: 'POST', body: JSON.stringify({ nodeId: destination.value, cpu: cpu.value, memoryMiB: Math.round(memoryGiB.value * 1024), diskMiB: Math.round(diskGiB.value * 1024) }) })
    saved.value = 'Move requested.'
  })
}
</script>

<template>
  <div v-if="relevant" class="border-b border-line px-6 py-2 text-xs text-muted" role="status" aria-live="polite">
    <span class="font-semibold">{{ node?.name || (run.nodeId === LOCAL_NODE_ID ? 'Master runner' : 'Unknown node') }}</span>
    <span v-if="run.resources"> · {{ run.resources.cpu }} CPU · {{ formatMiB(run.resources.memoryMiB) }} RAM</span>
    <span v-if="run.backup?.capturedAt" :title="`${new Date(run.backup.capturedAt).toLocaleString()}. Newer chat and file changes may not be in this recovery point.`"> · Recoverable VM state: {{ relativeAge(run.backup.capturedAt, now) }}</span>
    <span v-if="run.backup?.status === 'saving'"> · Saving changes…</span>
    <span v-if="run.pinnedNodeId"> · Fixed node; automatic failover disabled</span>
    <p v-if="run.nodeState" class="mt-1">
      {{ labels[run.nodeState] || run.nodeState }}
    </p>
    <p v-if="run.capacityWaitUntil" class="mt-1">
      Waiting for capacity until {{ new Date(run.capacityWaitUntil).toLocaleString() }}.
    </p>
    <p v-if="run.backup?.error" class="mt-1 text-coral">
      Backup: {{ run.backup.error }}
    </p>
    <p v-if="run.restoredAt" class="mt-1">
      Resumed from a recovery point of {{ new Date(run.restoredAt).toLocaleString() }}. More recent messages remain visible; restored files can be older.
    </p>
    <p v-if="run.movementError" class="mt-1 text-coral">
      {{ run.movementError }}
    </p>
    <details class="mt-2">
      <summary>Execution node</summary>
      <fieldset class="mt-2">
        <legend class="font-semibold">
          Where it runs next time
        </legend>
        <div class="mt-1 flex flex-wrap items-center gap-2">
          <label>Placement <select v-model="mode" aria-label="Placement" :disabled="busy" class="rounded border border-line bg-surface p-1"><option value="automatic">Automatic</option><option value="preferred">Prefer a node</option><option value="fixed">Fix to a node</option></select></label>
          <label v-if="mode !== 'automatic'">Node <select v-model="selection" aria-label="Node" :disabled="busy" class="rounded border border-line bg-surface p-1"><option value="" disabled>Select a node</option><option v-for="candidate in nodes" :key="candidate.id" :value="candidate.id">{{ candidate.name }}</option></select></label>
          <UiButton size="small" :disabled="busy || (mode !== 'automatic' && !selection)" @click="savePreference">
            Save preference
          </UiButton>
        </div>
        <p class="mt-1">
          Automatic picks the authorized node with the most free CPU and RAM. A preference still permits failover; a fixed node waits for that machine. This does not move the conversation now.
        </p>
      </fieldset>
      <fieldset v-if="destinations.length" class="mt-3">
        <legend class="font-semibold">
          Move to another node
        </legend>
        <div class="mt-1 flex flex-wrap items-center gap-2">
          <label>Destination <select v-model="destination" aria-label="Destination" :disabled="busy" class="rounded border border-line bg-surface p-1"><option value="" disabled>Select a node</option><option v-for="candidate in destinations" :key="candidate.id" :value="candidate.id">{{ candidate.name }}</option></select></label>
          <label>CPU <input v-model.number="cpu" class="w-16 rounded border border-line bg-surface p-1" type="number" min="1" :disabled="busy"></label>
          <label>RAM (GiB) <input v-model.number="memoryGiB" class="w-20 rounded border border-line bg-surface p-1" type="number" min="0.125" step="any" :disabled="busy"></label>
          <label>Disk (GiB) <input v-model.number="diskGiB" class="w-20 rounded border border-line bg-surface p-1" type="number" :min="minimumDiskGiB" step="any" :disabled="busy"></label>
          <UiButton size="small" :disabled="!canMove" @click="move">
            Move now
          </UiButton>
        </div>
        <p class="mt-1">
          Reserves the destination, pauses the conversation, transfers its environment and resumes it there. Running commands are interrupted. The disk cannot shrink.
        </p>
      </fieldset>
      <p v-if="saved" class="mt-1">
        {{ saved }}
      </p>
      <p v-if="error" role="alert" class="mt-1 text-coral">
        {{ error }}
      </p>
    </details>
  </div>
</template>
