<script setup lang="ts">
import type { Run } from '../../shared/contracts'
import type { ExecutionNode } from '../../shared/nodes'
import type { Placement } from '../placement'
import {
  computed,
  nextTick,
  onUnmounted,
  ref,
  watch,
} from 'vue'
import {
  DEFAULT_RESOURCES,
  formatBytes,
  formatMiB,
  LOCAL_NODE_ID,
  relativeAge,
} from '../../shared/nodes'
import { api } from '../api'
import {
  Check,
  LoaderCircle,
  Server,
  TriangleAlert,
  X,
} from '../icons'
import { loadPlacement } from '../placement'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'

type Mode = 'automatic' | 'preferred' | 'fixed'

const props = defineProps<{ run: Run }>()
const nodes = ref<ExecutionNode[]>([])
const labels: Record<string, string> = {
  'pausing': 'Pausing the VM',
  'saving': 'Saving the environment',
  'restoring': 'Restoring the environment',
  'resuming': 'Resuming the conversation',
  'waiting-for-node': 'Waiting for a compatible node',
  'updating': 'Updating the node',
}
const storageLabels: Record<string, string> = {
  'storage-unavailable': 'Waiting for storage; resumes automatically',
  'disk-space': 'Paused until disk space is available',
  'backup-lag': 'Paused while changes are saved',
  'integrity': 'Paused: disk integrity needs attention',
}
const storageShort: Record<string, string> = {
  'storage-unavailable': 'Waiting for storage',
  'disk-space': 'Disk full',
  'backup-lag': 'Saving before resuming',
  'integrity': 'Disk needs attention',
}
const modes: { value: Mode, label: string }[] = [{ value: 'automatic', label: 'Automatic' }, { value: 'preferred', label: 'Prefer a node' }, { value: 'fixed', label: 'Fix to a node' }]
const modeHelp: Record<Mode, string> = { automatic: 'Picks the authorized node with the most free CPU and RAM.', preferred: 'Uses this node when it is available, otherwise fails over to another.', fixed: 'Waits for this machine; no automatic failover.' }
const node = computed(() => nodes.value.find(node => node.id === props.run.nodeId))
const nodeName = computed(() => node.value?.name || (props.run.nodeId === LOCAL_NODE_ID ? 'Master runner' : 'Unknown node'))
const selection = ref('')
const mode = ref<Mode>('automatic')
const savedPlacement = ref({ mode: 'automatic' as Mode, selection: '' })
const destination = ref('')
const cpu = ref(DEFAULT_RESOURCES.cpu)
const memoryGiB = ref(DEFAULT_RESOURCES.memoryMiB / 1024)
const diskGiB = ref(DEFAULT_RESOURCES.diskMiB / 1024)
const busy = ref(false)
const loaded = ref(false)
const error = ref('')
const saved = ref('')
const open = ref(false)
const moving = ref(false)
const chip = ref<HTMLButtonElement>()
const panel = ref<HTMLDialogElement>()
const position = ref({ left: '16px', top: '80px', maxHeight: 'calc(100dvh - 96px)' })
// Recovery ages are shown to the minute, so a slow clock is enough.
const now = ref(Date.now())
const timer = setInterval(() => {
  now.value = Date.now()
}, 30_000)
// With only the master runner there is nothing to choose, so stay out of the way unless something happens.
const relevant = computed(() => (!!props.run.nodeId && props.run.nodeId !== LOCAL_NODE_ID)
  || props.run.storage?.mode === 'on-demand'
  || nodes.value.some(candidate => !candidate.local)
  || !!error.value
  || !!(props.run.nodeState || props.run.movementError || props.run.restoredAt || props.run.capacityWaitUntil || props.run.backup?.error))
const destinations = computed(() => nodes.value.filter(candidate => candidate.id !== props.run.nodeId))
const minimumDiskGiB = computed(() => (props.run.resources?.diskMiB ?? 128) / 1024)
const canMove = computed(() => Number.isInteger(cpu.value) && cpu.value > 0 && Number.isFinite(memoryGiB.value) && memoryGiB.value >= 0.125 && Number.isFinite(diskGiB.value) && diskGiB.value >= minimumDiskGiB.value && !!props.run.sessionId && !busy.value && !!destination.value && destination.value !== props.run.nodeId && ['running', 'succeeded'].includes(props.run.status) && !props.run.nodeState)
const changed = computed(() => mode.value !== savedPlacement.value.mode || (mode.value !== 'automatic' && selection.value !== savedPlacement.value.selection))
const dirtyBytes = computed(() => props.run.storage?.dirtyBytes || 0)
const localBytes = computed(() => props.run.storage?.localBytes)
// The chip carries one state: the most serious thing going on, so a failure never hides behind a routine save.
const status = computed((): { tone: 'bad' | 'warn' | 'busy' | 'ok' | 'idle', label: string } => {
  const run = props.run
  const waiting = run.storage?.waitingFor
  if (run.movementError)
    return { tone: 'bad', label: 'Move failed' }
  if (run.backup?.error)
    return { tone: 'bad', label: 'Sync failed' }
  if (waiting)
    return { tone: waiting === 'integrity' ? 'bad' : 'warn', label: storageShort[waiting] || waiting }
  if (run.nodeState)
    return { tone: 'busy', label: labels[run.nodeState] || run.nodeState }
  if (run.capacityWaitUntil)
    return { tone: 'warn', label: 'Waiting for capacity' }
  if (run.backup?.status === 'saving')
    return { tone: 'warn', label: 'Saving…' }
  if (dirtyBytes.value)
    return { tone: 'warn', label: `${formatBytes(dirtyBytes.value)} unsaved` }
  if (run.backup?.capturedAt)
    return { tone: 'ok', label: `Synced ${relativeAge(run.backup.capturedAt, now.value)}` }
  return { tone: 'idle', label: 'No sync yet' }
})
const chipTone = {
  bad: 'border-coral/40 bg-coral-soft text-coral',
  warn: 'border-warning/40 bg-warning-surface text-warning',
  busy: 'border-line bg-inset text-accent',
  ok: 'border-line bg-inset text-muted',
  idle: 'border-line bg-inset text-muted',
}

function applyPlacement(value: Placement) {
  nodes.value = value.nodes
  mode.value = value.pinnedNodeId ? 'fixed' : value.preferredNodeId ? 'preferred' : 'automatic'
  selection.value = value.pinnedNodeId || value.preferredNodeId || props.run.nodeId || ''
  savedPlacement.value = { mode: mode.value, selection: selection.value }
  loaded.value = true
}

watch(() => props.run.id, async (runId, _, onCleanup) => {
  let cancelled = false
  onCleanup(() => {
    cancelled = true
  })
  error.value = ''
  saved.value = ''
  loaded.value = false
  const resources = props.run.resources || DEFAULT_RESOURCES
  cpu.value = resources.cpu
  memoryGiB.value = resources.memoryMiB / 1024
  diskGiB.value = resources.diskMiB / 1024
  open.value = false
  moving.value = false
  try {
    const value = await loadPlacement(runId)
    if (cancelled)
      return
    applyPlacement(value)
    destination.value = destinations.value[0]?.id || ''
  }
  catch (e) {
    if (!cancelled)
      error.value = e instanceof Error ? e.message : 'Unable to load placement'
  }
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
  const desired = { mode: mode.value, selection: selection.value }
  return request(async () => {
    await api(`/nodes/placement/${props.run.id}`, { method: 'PUT', body: JSON.stringify({ pinnedNodeId: mode.value === 'fixed' ? selection.value : null, preferredNodeId: mode.value === 'preferred' ? selection.value : null }) })
    savedPlacement.value = desired
    saved.value = 'Preference saved. It applies the next time this conversation starts or recovers.'
  })
}

function move() {
  return request(async () => {
    await api(`/nodes/placement/${props.run.id}/move`, {
      method: 'POST',
      body: JSON.stringify({
        nodeId: destination.value,
        cpu: cpu.value,
        memoryMiB: Math.round(memoryGiB.value * 1024),
        diskMiB: Math.round(diskGiB.value * 1024),
      }),
    })
    moving.value = false
    saved.value = 'Move requested.'
  })
}

function close(returnFocus = false) {
  panel.value?.close()
  open.value = false
  if (returnFocus)
    chip.value?.focus()
}

async function toggle() {
  if (open.value) {
    close(true)
    return
  }

  open.value = true
  await nextTick()
  const rect = chip.value?.getBoundingClientRect()
  const top = Math.min((rect?.bottom ?? 60) + 8, window.innerHeight - 220)
  position.value = { maxHeight: `${window.innerHeight - top - 16}px`, left: `${Math.max(16, Math.min(rect?.left ?? 16, window.innerWidth - 416))}px`, top: `${top}px` }
  panel.value?.showModal()
  panel.value?.focus()
  if (!changed.value && !busy.value) {
    const runId = props.run.id
    loaded.value = false
    try {
      const value = await loadPlacement(runId)
      if (props.run.id !== runId)
        return
      applyPlacement(value)
      error.value = ''
    }
    catch (e) {
      if (props.run.id === runId)
        error.value = e instanceof Error ? e.message : 'Unable to load placement'
    }
  }
}

function resized() {
  if (!moving.value)
    close(true)
}

window.addEventListener('resize', resized)
onUnmounted(() => {
  clearInterval(timer)
  window.removeEventListener('resize', resized)
})
</script>

<template>
  <div v-if="relevant" class="relative inline-flex max-w-full" @keydown.esc.stop="close(true)">
    <button
      ref="chip"
      type="button"
      class="flex min-w-0 max-w-full items-center gap-1.5 rounded-full border py-0.5 pr-2.5 pl-2 text-[11px] leading-5 transition-colors hover:border-control hover:text-ink"
      :class="chipTone[status.tone]"
      aria-haspopup="dialog"
      :aria-expanded="open"
      aria-label="Execution node"
      @click="toggle"
    >
      <Icon :name="Server" :size="13" class="shrink-0" />
      <span class="truncate">{{ nodeName }}<template v-if="run.resources"> · {{ run.resources.cpu }} CPU · {{ formatMiB(run.resources.memoryMiB) }}</template></span>
      <template v-if="status.label">
        <span aria-hidden="true" class="opacity-40">|</span>
        <Icon
          v-if="status.tone === 'busy' || (status.tone === 'warn' && run.backup?.status === 'saving')"
          :name="LoaderCircle"
          :size="12"
          class="shrink-0 animate-spin motion-reduce:animate-none"
        />
        <Icon
          v-else-if="status.tone === 'bad' || status.tone === 'warn'"
          :name="TriangleAlert"
          :size="12"
          class="shrink-0"
        />
        <Icon
          v-else-if="status.tone === 'ok'"
          :name="Check"
          :size="12"
          class="shrink-0"
        />
        <span role="status" class="whitespace-nowrap">{{ status.label }}</span>
      </template>
    </button>
    <Teleport to="body">
      <dialog
        v-if="open"
        ref="panel"
        :style="position"
        aria-label="Execution node"
        tabindex="-1"
        class="fixed m-0 max-h-[calc(100dvh-160px)] w-[min(400px,calc(100vw-32px))] overflow-auto rounded-card border border-control bg-raised p-4 text-xs text-muted shadow-lift outline-none"
        @cancel.prevent="close(true)"
        @click="($event.target === panel) && close(true)"
      >
        <section class="flex items-start justify-between gap-3">
          <div class="min-w-0">
            <h2 class="m-0! text-sm! font-semibold text-ink">
              {{ nodeName }}
            </h2>
            <p class="m-0! mt-0.5!">
              <template v-if="run.resources">
                {{ run.resources.cpu }} CPU · {{ formatMiB(run.resources.memoryMiB) }} RAM
              </template>
              <template v-if="run.storage?.mode === 'on-demand'">
                · files load on demand
              </template>
            </p>
            <p v-if="savedPlacement.mode === 'fixed'" class="m-0! mt-0.5!">
              Fixed node; automatic failover disabled
            </p>
          </div>
          <div class="flex items-center gap-1">
            <UiButton
              v-if="destinations.length"
              size="small"
              :aria-expanded="moving"
              @click="moving = true"
            >
              Move…
            </UiButton><button
              type="button"
              class="rounded-lg p-2 hover:bg-hover"
              aria-label="Close execution details"
              @click="close(true)"
            >
              <Icon :name="X" :size="16" />
            </button>
          </div>
        </section>

        <section class="mt-3 border-t border-line pt-3">
          <div class="flex items-center justify-between gap-3">
            <h3 class="m-0! text-xs! font-semibold text-ink">
              Workspace sync
            </h3>
            <span v-if="status.label" :class="status.tone === 'bad' ? 'text-coral' : status.tone === 'warn' ? 'text-warning' : status.tone === 'busy' ? 'text-accent' : ''">{{ status.label }}</span>
          </div>
          <template v-if="localBytes !== undefined">
            <div class="mt-2 flex h-1.5 overflow-hidden rounded-full bg-inset" aria-hidden="true">
              <span class="bg-accent" :style="{ width: `${localBytes ? 100 * Math.max(0, localBytes - dirtyBytes) / localBytes : 0}%` }" />
              <span :class="run.backup?.error ? 'bg-coral' : 'bg-warning'" :style="{ width: `${localBytes ? 100 * Math.min(dirtyBytes, localBytes) / localBytes : 0}%` }" />
            </div>
            <p class="m-0! mt-1.5! flex flex-wrap gap-x-3">
              <span>{{ formatBytes(localBytes) }} on this node</span>
              <span v-if="dirtyBytes">{{ formatBytes(dirtyBytes) }} not yet saved</span>
            </p>
          </template>
          <p v-if="dirtyBytes" class="m-0! mt-1.5!">
            {{ run.storage?.dirtySince ? `Unsaved changes since ${relativeAge(run.storage.dirtySince, now)}` : 'Unsaved changes' }}; they exist only on this node until saved.
          </p>
          <p v-else-if="run.backup?.capturedAt" class="m-0! mt-1.5!" :title="`${new Date(run.backup.capturedAt).toLocaleString()}. Newer disk changes may still be waiting for synchronization.`">
            Disk synchronized {{ relativeAge(run.backup.capturedAt, now) }}.
          </p>
          <p v-if="run.storage?.waitingFor" class="m-0! mt-1.5!" :class="run.storage.waitingFor === 'integrity' ? 'text-coral' : 'text-warning'">
            {{ storageLabels[run.storage.waitingFor] || run.storage.waitingFor }}.
          </p>
          <p v-if="run.nodeState" class="m-0! mt-1.5!">
            {{ labels[run.nodeState] || run.nodeState }}
          </p>
          <p v-if="run.capacityWaitUntil" class="m-0! mt-1.5!">
            Waiting for capacity until {{ new Date(run.capacityWaitUntil).toLocaleString() }}.
          </p>
          <p v-if="run.restoredAt" class="m-0! mt-1.5!">
            Resumed from a recovery point of {{ new Date(run.restoredAt).toLocaleString() }}. More recent messages remain visible; restored files can be older.
          </p>
          <p v-if="run.backup?.error" class="m-0! mt-2! flex items-start gap-2 rounded-lg bg-coral-soft px-2.5 py-2 text-coral">
            <Icon :name="TriangleAlert" :size="13" class="mt-0.5 shrink-0" /><span>Synchronization: {{ run.backup.error }}</span>
          </p>
          <p v-if="run.movementError" class="m-0! mt-2! flex items-start gap-2 rounded-lg bg-coral-soft px-2.5 py-2 text-coral">
            <Icon :name="TriangleAlert" :size="13" class="mt-0.5 shrink-0" /><span>{{ run.movementError }}</span>
          </p>
        </section>

        <fieldset :disabled="busy || !loaded" class="mt-3 border-t border-line pt-3">
          <legend class="float-left mb-2 w-full text-xs font-semibold text-ink">
            Next start runs on
          </legend>
          <UiSegments
            v-model="mode"
            label="Placement"
            :options="modes"
            class="rounded-lg border border-line bg-inset p-0.5"
          />
          <select
            v-if="mode !== 'automatic'"
            v-model="selection"
            aria-label="Node"
            :disabled="busy"
            class="mt-2 w-full rounded-lg border border-control bg-inset p-1.5 text-xs text-ink"
          >
            <option value="" disabled>
              Select a node
            </option>
            <option v-for="candidate in nodes" :key="candidate.id" :value="candidate.id">
              {{ candidate.name }}
            </option>
          </select>
          <p class="m-0! mt-2!">
            {{ modeHelp[mode] }} This does not move the conversation now.
          </p>
          <div class="mt-3 flex justify-end">
            <UiButton
              size="small"
              variant="primary"
              :disabled="!loaded || busy || !changed || (mode !== 'automatic' && !selection)"
              @click="savePreference"
            >
              Save preference
            </UiButton>
          </div>
        </fieldset>

        <p v-if="saved" role="status" class="m-0! mt-3!">
          {{ saved }}
        </p>
        <p v-if="error" role="alert" class="m-0! mt-3! text-coral">
          {{ error }}
        </p>
      </dialog>
    </Teleport>
    <Modal v-if="moving && destinations.length" title="Move to another node" @close="moving = false">
      <form class="p-6 grid gap-4" @submit.prevent="move">
        <div class="grid grid-cols-2 gap-2">
          <label class="col-span-2 grid gap-1">Destination <select v-model="destination" :disabled="busy" class="rounded-lg border border-control bg-inset p-1.5 text-ink"><option value="" disabled>Select a node</option><option v-for="candidate in destinations" :key="candidate.id" :value="candidate.id">{{ candidate.name }}</option></select></label>
          <label class="grid gap-1">CPU <input
            v-model.number="cpu"
            class="rounded-lg border border-control bg-inset p-1.5 text-ink"
            type="number"
            min="1"
            :disabled="busy"
          ></label>
          <label class="grid gap-1">RAM (GiB) <input
            v-model.number="memoryGiB"
            class="rounded-lg border border-control bg-inset p-1.5 text-ink"
            type="number"
            min="0.125"
            step="any"
            :disabled="busy"
          ></label>
          <label class="grid gap-1">Disk (GiB) <input
            v-model.number="diskGiB"
            class="rounded-lg border border-control bg-inset p-1.5 text-ink"
            type="number"
            :min="minimumDiskGiB"
            step="any"
            :disabled="busy"
          ></label>
        </div>
        <p class="m-0! mt-2!">
          Reserves the destination, pauses the conversation, transfers its environment and resumes it there. Running commands are interrupted. The disk cannot shrink.
        </p>
        <div class="mt-3 flex justify-end">
          <UiButton size="small" :disabled="!canMove" type="submit">
            Move now
          </UiButton>
        </div>
        <p v-if="error" role="alert" class="text-coral">
          {{ error }}
        </p>
      </form>
    </Modal>
  </div>
</template>
