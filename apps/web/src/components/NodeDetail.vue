<script setup lang="ts">
import type { ExecutionNode } from '../../../../packages/contracts/nodes'
import { computed, ref } from 'vue'
import {
  formatBytes,
  formatMiB,
  nativeModeLabel,
  nodeDiagnostics,
  nodeHealth,
  usedPercent,
} from '../../../../packages/contracts/nodes'
import { nodeBackend } from '../agent-access'
import { state } from '../api'
import { Server, Settings2 } from '../icons'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import NativeNodeMode from './NativeNodeMode.vue'
import NodeHealthBadge from './NodeHealthBadge.vue'
import NodeMeter from './NodeMeter.vue'
import NodeStorage from './NodeStorage.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'

// Everything about one machine, in four sections: how it is doing, who may use it, its disks, and its settings.
// Editors and confirmations stay in their own dialogs, opened above this panel by the page.
const props = defineProps<{ node: ExecutionNode, busy: boolean, error?: string }>()
const emit = defineEmits<{
  close: []
  changed: []
  configure: []
  grant: []
  clean: []
  release: []
  switch: []
  revoke: []
}>()

type Section = 'overview' | 'access' | 'storage' | 'settings'

const section = ref<Section>('overview')
const sections: { value: Section, label: string }[] = [
  { value: 'overview', label: 'Overview' },
  { value: 'access', label: 'Access' },
  { value: 'storage', label: 'Storage' },
  { value: 'settings', label: 'Settings' },
]
const health = computed(() => nodeHealth(props.node))
const diagnostics = computed(() => nodeDiagnostics(props.node))
const native = computed(() => props.node.capabilities.backend === 'native')
const slots = computed(() => props.node.slots ?? 0)
const free = computed(() => props.node.availableSlots ?? 0)
const runtime = computed(() => `${props.node.local ? 'Built-in runner · ' : ''}${native.value ? 'Native process · shares the host GPU' : 'Firecracker VM'}`)
const banner = computed(() => ({
  ready: 'bg-success/10 text-success',
  full: 'bg-warning-surface text-warning',
  busy: 'bg-warning-surface text-warning',
  updating: 'bg-warning-surface text-warning',
  setup: 'bg-warning-surface text-warning',
  reserved: 'bg-power-surface text-power',
  paused: 'bg-hover text-muted',
  revoked: 'bg-hover text-muted',
  attention: 'bg-coral-soft text-coral',
  offline: 'bg-coral-soft text-coral',
})[health.value.state])
const exclusive = computed(() => (props.node.exclusiveAgents ?? []).map(id => state.agents.find(agent => agent.id === id)?.name ?? 'Removed agent'))
// The larger of the two free-space reserves applies.
const reserve = computed(() => {
  const storage = props.node.storage
  return storage ? `${formatMiB(storage.reserveMiB)} or ${storage.reservePercent}%, whichever is larger` : ''
})
</script>

<template>
  <Modal :title="node.name" drawer @close="emit('close')">
    <template #heading>
      <span class="flex min-w-0 items-center gap-3.5">
        <span class="grid size-11 shrink-0 place-items-center rounded-xl bg-inset text-accent"><Icon :name="Server" :size="20" /></span>
        <span class="min-w-0">
          <span class="block truncate font-heading text-[19px] leading-tight font-extrabold tracking-[-0.02em]">{{ node.name }}</span>
          <span class="mt-0.5 block truncate text-xs font-normal text-muted">{{ runtime }}</span>
        </span>
      </span>
    </template>
    <div class="flex flex-1 flex-col">
      <div v-if="error" class="px-6.5 pt-3 phone:px-5">
        <UiAlert>{{ error }}</UiAlert>
      </div>
      <template v-if="node.revoked">
        <section class="grid gap-3 px-6.5 py-5 phone:px-5">
          <span><NodeHealthBadge :health="health" /></span>
          <p class="text-sm text-muted">
            This machine no longer has access. Register it again with a new code to reconnect it.
          </p>
        </section>
      </template>
      <template v-else>
        <div class="border-b border-line px-6.5 py-3 phone:px-5">
          <UiSegments
            v-model="section"
            label="Node sections"
            :options="sections"
            stretch
          />
        </div>

        <template v-if="section === 'overview'">
          <section class="grid gap-3 border-b border-line px-6.5 py-5 phone:px-5" aria-label="Health">
            <div class="rounded-xl px-4 py-3" :class="banner" role="status">
              <p class="text-sm font-semibold">
                {{ health.label }}
              </p>
              <p class="text-xs text-ink/80">
                {{ health.detail }}
              </p>
            </div>
            <p v-if="native && node.nativeModeProposal" class="text-xs">
              An agent requested {{ nativeModeLabel(node.nativeModeProposal.mode) }}.
              <button type="button" class="font-semibold text-accent hover:underline" @click="section = 'access'">
                Review the request
              </button>
            </p>
            <ul v-if="diagnostics.length" class="grid list-disc gap-1 pl-5 text-xs text-coral" :aria-label="`Why ${node.name} cannot take new work`">
              <li v-for="reason in diagnostics" :key="reason">
                {{ reason }}
              </li>
            </ul>
            <p v-if="node.maintenance" class="text-xs" role="status">
              {{ node.maintenance === 'draining' ? 'Pausing and saving conversations for an update' : 'Ready to restart for the update' }}
            </p>
            <p v-if="node.maintenance && node.maintenanceError" role="alert" class="text-xs text-coral">
              {{ node.maintenanceError }}
            </p>
            <div v-if="node.exclusiveReservation" role="status" class="grid gap-1 rounded-xl border border-line p-3.5 text-xs">
              <p class="text-sm font-semibold">
                Exclusive reservation · {{ node.exclusiveReservation.state }}
              </p>
              <p>{{ node.exclusiveReservation.reason }}</p>
              <p class="text-muted">
                {{ node.exclusiveReservation.pausedRunIds.length }} conversations suspended<span v-if="node.exclusiveReservation.expiresAt"> · until {{ new Date(node.exclusiveReservation.expiresAt).toLocaleTimeString() }}</span>
              </p>
              <div class="mt-1.5 flex flex-wrap items-center gap-3">
                <RouterLink :to="`/runs/${node.exclusiveReservation.ownerRunId}`" class="font-semibold text-accent hover:underline">
                  View reservation owner
                </RouterLink>
                <UiButton size="small" :disabled="busy || node.exclusiveReservation.state === 'releasing'" @click="emit('release')">
                  Release reservation
                </UiButton>
              </div>
            </div>
          </section>

          <section class="border-b border-line px-6.5 py-5 phone:px-5" aria-labelledby="node-capacity">
            <h3 id="node-capacity" class="eyebrow mb-3.5">
              Shared capacity
            </h3>
            <dl class="grid gap-3 text-sm">
              <div class="grid grid-cols-[4.5rem_minmax(0,1fr)] items-center gap-3">
                <dt class="text-muted">
                  Slots
                </dt>
                <dd class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3">
                  <NodeMeter v-if="slots" :used="usedPercent(slots - free, slots)" :label="`${node.name} slots in use`" />
                  <span v-else />
                  <span class="text-right tabular-nums">
                    <b class="font-semibold">{{ free }}</b> <span class="text-xs text-muted">of {{ slots }} free</span>
                  </span>
                </dd>
              </div>
              <div class="grid grid-cols-[4.5rem_minmax(0,1fr)] items-center gap-3">
                <dt class="text-muted">
                  CPU
                </dt>
                <dd class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3">
                  <span />
                  <span class="text-right tabular-nums">
                    <b class="font-semibold">{{ node.limits.cpu }}</b> <span class="text-xs text-muted">cores</span>
                  </span>
                </dd>
              </div>
              <div class="grid grid-cols-[4.5rem_minmax(0,1fr)] items-center gap-3">
                <dt class="text-muted">
                  Memory
                </dt>
                <dd class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3">
                  <NodeMeter v-if="node.usage" :used="usedPercent(node.usage.memoryMiB, node.limits.memoryMiB)" :label="`${node.name} RAM in use`" />
                  <span v-else />
                  <span class="text-right tabular-nums">
                    <template v-if="node.usage">
                      <b class="font-semibold">{{ formatMiB(node.usage.memoryMiB) }}</b> <span class="text-xs text-muted">of {{ formatMiB(node.limits.memoryMiB) }}</span>
                    </template>
                    <template v-else>
                      <b class="font-semibold">{{ formatMiB(node.limits.memoryMiB) }}</b> <span class="text-xs text-muted">· usage unavailable</span>
                    </template>
                  </span>
                </dd>
              </div>
              <div class="grid grid-cols-[4.5rem_minmax(0,1fr)] items-center gap-3">
                <dt class="text-muted">
                  Disk
                </dt>
                <dd class="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3">
                  <NodeMeter v-if="node.usage" :used="usedPercent(node.usage.diskMiB, node.limits.diskMiB)" :label="`${node.name} disk in use`" />
                  <span v-else />
                  <span class="text-right tabular-nums">
                    <template v-if="node.usage">
                      <b class="font-semibold">{{ formatMiB(node.usage.diskMiB) }}</b> <span class="text-xs text-muted">of {{ formatMiB(node.limits.diskMiB) }}</span>
                    </template>
                    <template v-else>
                      <b class="font-semibold">{{ formatMiB(node.limits.diskMiB) }}</b> <span class="text-xs text-muted">· usage unavailable</span>
                    </template>
                  </span>
                </dd>
              </div>
            </dl>
            <p class="mt-3 text-xs text-muted">
              Limits set for Cairn, shared by all slots. {{ node.occupiedSlots ?? 0 }} active or reserved.
            </p>
          </section>

          <section class="px-6.5 py-5 phone:px-5" aria-labelledby="node-machine">
            <h3 id="node-machine" class="eyebrow mb-3.5">
              Machine
            </h3>
            <dl class="grid grid-cols-[6.5rem_minmax(0,1fr)] gap-x-3 gap-y-2 text-xs">
              <dt class="text-muted">
                Detected
              </dt>
              <dd>{{ node.capabilities.cpu }} CPU · {{ formatMiB(node.capabilities.memoryMiB) }} RAM · {{ formatMiB(node.capabilities.diskMiB) }} disk</dd>
              <dt class="text-muted">
                Platform
              </dt>
              <dd>{{ node.capabilities.os }} {{ node.capabilities.arch }} · {{ native ? `ublk ${node.capabilities.ublk ? 'available' : 'unavailable'}` : `KVM ${node.capabilities.kvm ? 'available' : 'unavailable'}` }}</dd>
              <template v-if="node.tags.length || node.systemTags?.length">
                <dt class="text-muted">
                  Tags
                </dt>
                <dd class="flex flex-wrap gap-1.5">
                  <span v-for="tag in node.tags" :key="`tag-${tag}`" class="rounded-full bg-hover px-2 py-0.5 font-semibold">{{ tag }}</span>
                  <span
                    v-for="tag in node.systemTags"
                    :key="`system-${tag}`"
                    class="rounded-full border border-line px-2 py-0.5 text-muted"
                    :title="`Detected tag ${tag}`"
                  >{{ tag }}</span>
                </dd>
              </template>
              <template v-if="node.lastSeen">
                <dt class="text-muted">
                  Last contact
                </dt>
                <dd>{{ new Date(node.lastSeen).toLocaleString() }}</dd>
              </template>
              <template v-if="node.imageDigest">
                <dt class="text-muted">
                  Version
                </dt>
                <dd class="break-all">
                  {{ node.imageDigest }}
                </dd>
              </template>
              <template v-if="node.runtimeId">
                <dt class="text-muted">
                  Runtime
                </dt>
                <dd class="break-all">
                  {{ node.runtimeId }}
                </dd>
              </template>
            </dl>
          </section>
        </template>

        <template v-else-if="section === 'access'">
          <section class="border-b border-line px-6.5 py-5 phone:px-5" :aria-label="`Agents on ${node.name}`">
            <div class="mb-3.5 flex items-baseline justify-between gap-3">
              <h3 class="eyebrow">
                Agents allowed
              </h3>
              <RouterLink to="/agents" class="text-xs font-semibold text-accent hover:underline">
                Manage access from the agents ›
              </RouterLink>
            </div>
            <ul v-if="node.agents?.length" class="flex flex-wrap gap-2" aria-label="Agents allowed">
              <li v-for="agent in node.agents" :key="agent.id" class="inline-flex items-center gap-2 rounded-full border border-line py-1 pr-3 pl-1 text-xs">
                <AgentAvatar
                  :name="agent.name"
                  :identity="agent.id"
                  :size="22"
                  class="rounded-full"
                />
                {{ agent.name }}<span v-if="agent.allNodes" class="text-muted">· all nodes</span>
              </li>
            </ul>
            <p v-else class="text-sm text-muted">
              No agent can run here yet.
            </p>
            <template v-if="exclusive.length">
              <h3 class="eyebrow mt-5 mb-2">
                Can reserve it exclusively
              </h3>
              <p class="text-sm">
                {{ exclusive.join(', ') }}
              </p>
            </template>
            <p class="mt-4 text-xs text-muted">
              Access is set on each agent’s page.
            </p>
          </section>
          <section v-if="native" class="px-6.5 py-5 phone:px-5" aria-labelledby="node-native">
            <h3 id="node-native" class="eyebrow mb-3.5">
              Native execution
            </h3>
            <p class="text-sm">
              {{ node.nativeEnabled ? 'Enabled' : 'Disabled' }} · {{ node.nativeAccounts?.length ?? 0 }} {{ node.nativeAccounts?.length === 1 ? 'trusted person' : 'trusted people' }}
              <button type="button" class="ml-1 text-xs font-semibold text-accent hover:underline" @click="emit('configure')">
                Change
              </button>
            </p>
            <NativeNodeMode :node="node" @changed="emit('changed')" />
            <UiButton v-if="node.nativeModeRequest?.mode === 'administrator'" @click="emit('grant')">
              Review agent access to this host
            </UiButton>
          </section>
        </template>

        <template v-else-if="section === 'storage'">
          <section v-if="node.diskAccounting" class="border-b border-line px-6.5 py-5 phone:px-5" aria-label="Disk capacity details">
            <h3 class="eyebrow mb-3.5">
              Disk capacity
            </h3>
            <dl class="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-2 text-sm">
              <dt class="text-muted">
                Stored locally
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.localBytes) }}
              </dd>
              <dt class="text-muted">
                Granted but unused
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.unusedGrantedBytes) }}
              </dd>
              <dt class="text-muted">
                Free on the host
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.hostFreeBytes) }}
              </dd>
              <dt class="text-muted">
                Kept free for the host
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.hostReserveBytes) }}
              </dd>
              <dt class="text-muted">
                Pool reserve
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.poolReserveBytes) }}
              </dd>
              <dt class="text-muted">
                Reclaimable
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.reclaimableBytes) }}
              </dd>
              <dt class="text-muted">
                Not yet synchronized
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatBytes(node.diskAccounting.unsynchronizedBytes) }}
              </dd>
            </dl>
            <p class="mt-3 text-xs text-muted">
              These figures overlap. Reclaimable space comes from synchronized inactive conversations; unsynchronized journal data is kept until it is published. Under pressure, inactive copies are freed without the one-hour delay, and waiting conversations take priority over further growth.
            </p>
          </section>
          <section v-if="node.staleDisks?.count" class="flex items-center justify-between gap-3 border-b border-line px-6.5 py-4 text-sm phone:px-5">
            <span>Old disks: {{ formatMiB(node.staleDisks.diskMiB) }} from {{ node.staleDisks.count }} conversation{{ node.staleDisks.count > 1 ? 's' : '' }}</span>
            <UiButton size="small" @click="emit('clean')">
              Free old disks
            </UiButton>
          </section>
          <section class="px-6.5 py-5 phone:px-5" aria-labelledby="node-storage-policy">
            <h3 id="node-storage-policy" class="eyebrow mb-3.5">
              Storage settings
            </h3>
            <dl v-if="node.storage" class="mb-4 grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-2 text-sm">
              <dt class="text-muted">
                Memory cache
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatMiB(node.storage.memoryCacheMiB) }}
              </dd>
              <dt class="text-muted">
                Disk cache budget
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatMiB(node.storage.cacheMiB) }}
              </dd>
              <dt class="text-muted">
                Minimum free disk
              </dt>
              <dd class="text-right tabular-nums">
                {{ reserve }}
              </dd>
              <dt class="text-muted">
                Synchronization target
              </dt>
              <dd class="text-right tabular-nums">
                {{ node.storage.backupSeconds }} s
              </dd>
            </dl>
            <NodeStorage :node="node" @saved="emit('changed')" />
          </section>
        </template>

        <template v-else>
          <section class="border-b border-line px-6.5 py-5 phone:px-5" aria-labelledby="node-settings">
            <h3 id="node-settings" class="eyebrow mb-3.5">
              Configuration
            </h3>
            <dl class="mb-4 grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-2 text-sm">
              <dt class="text-muted">
                Tags
              </dt>
              <dd class="text-right">
                {{ node.tags.length ? node.tags.join(' · ') : 'None' }}
              </dd>
              <dt class="text-muted">
                Execution slots
              </dt>
              <dd class="text-right tabular-nums">
                {{ slots }}
              </dd>
              <dt class="text-muted">
                Shared CPU budget
              </dt>
              <dd class="text-right tabular-nums">
                {{ node.limits.cpu }} of {{ node.capabilities.cpu }} cores
              </dd>
              <dt class="text-muted">
                Shared RAM budget
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatMiB(node.limits.memoryMiB) }}
              </dd>
              <dt class="text-muted">
                Shared disk budget
              </dt>
              <dd class="text-right tabular-nums">
                {{ formatMiB(node.limits.diskMiB) }}
              </dd>
              <dt class="text-muted">
                New work
              </dt>
              <dd class="text-right">
                {{ node.accepting ? 'Accepted' : 'Paused' }}
              </dd>
            </dl>
            <UiButton @click="emit('configure')">
              <Icon :name="Settings2" :size="15" /> Configure
            </UiButton>
          </section>
          <section v-if="!node.local" class="px-6.5 py-5 phone:px-5" aria-labelledby="node-danger">
            <h3 id="node-danger" class="eyebrow mb-3.5 text-danger!">
              Danger zone
            </h3>
            <div class="grid gap-3 rounded-xl border border-danger/30 p-4">
              <div class="flex items-center justify-between gap-3">
                <p class="text-xs text-muted">
                  Enroll this machine again with the {{ nodeBackend(node) === 'native' ? 'Firecracker VM' : 'native process' }} runtime.
                </p>
                <UiButton size="small" @click="emit('switch')">
                  {{ nodeBackend(node) === 'native' ? 'Switch to VM' : 'Switch to native' }}
                </UiButton>
              </div>
              <div class="flex items-center justify-between gap-3 border-t border-line pt-3">
                <p class="text-xs text-muted">
                  Remove this machine’s access to Cairn.
                </p>
                <UiButton size="small" variant="danger-outline" @click="emit('revoke')">
                  Revoke
                </UiButton>
              </div>
            </div>
          </section>
        </template>
      </template>
    </div>
  </Modal>
</template>
