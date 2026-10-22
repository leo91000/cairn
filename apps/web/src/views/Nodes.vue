<script setup lang="ts">
import type { ExecutionNode, NodeEnrollment, NodeSyncSettings } from '../../../../packages/contracts/nodes'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import { diskCapacity, formatMiB, nodeDiagnostics } from '../../../../packages/contracts/nodes'
import { nodeBackend } from '../agent-access'
import {
  api,
  ApiError,
  notify,
  refresh,
  state,
} from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import NodeStorage from '../components/NodeStorage.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  Plus,
  Server,
  Settings2,
} from '../icons'

const nodes = ref<ExecutionNode[]>([])
const nativeCallers = ref<Array<{ id: string, email: string }>>([])
const enrollmentBackend = ref<'firecracker' | 'native'>('firecracker')
// Stops before deleting while a conversation disk is mounted and keeps the unit until the rest is gone, so it can be rerun.
const nativeUninstallCommand = `sudo sh -c 'if [ -e /etc/systemd/system/cairn-native-node.service ]; then systemctl disable --now cairn-native-node || exit; fi; mounts=$(findmnt -rno TARGET) || { echo "findmnt could not list mounts." >&2; exit 1; }; printf "%s\\n" "$mounts" | grep ^/var/lib/cairn-native/; match=$?; if [ "$match" -ne 1 ]; then echo "Mount safety could not be confirmed. Run sudo systemctl start cairn-native-node to release owned conversation disks, then retry." >&2; exit 1; fi; rm -rf /opt/cairn-native /var/lib/cairn-native && { userdel cairn-workload || ! id cairn-workload >/dev/null 2>&1; } && rm -f /etc/systemd/system/cairn-native-node.service && systemctl daemon-reload'`
// The node a guided backend switch replaces, while the new runtime's enrollment is pending.
const replacing = ref<ExecutionNode | null>(null)
// Once the replacement connects and its setup steps end, retiring the previous node is offered.
let replacement: { previous: string, next: string } | null = null
const retiring = ref<{ previous: ExecutionNode, next: ExecutionNode } | null>(null)
const uninstallCommands = {
  firecracker: 'sudo systemctl disable --now cairn-node; sudo docker rm -f cairn-execution-node; sudo rm -rf /etc/systemd/system/cairn-node.service /opt/cairn-node /var/lib/cairn-node',
  native: nativeUninstallCommand,
}
const backendNames = { firecracker: 'Firecracker VM', native: 'native process' }
const editing = ref<ExecutionNode | null>(null)
const tags = ref('')
const name = ref('')
const managerUrl = ref('')
// Ceilings are edited in GiB and stored in MiB.
const memoryGiB = ref(0)
const diskGiB = ref(0)
const adding = ref(false)
const enrollment = ref<NodeEnrollment | null>(null)
const enrollmentCommand = computed(() => {
  const origin = enrollment.value?.managerUrl
  return origin ? `sudo cairn node-enroll '${origin.replaceAll('\'', '\'\\\'\'')}' /var/lib/cairn-node/data/node` : ''
})
// Machines known when the code was created, to recognise the newly connected one.
let knownBeforeEnrollment = new Set<string>()
const error = ref('')
const busy = ref(false)
const recovery = ref<NodeSyncSettings | null>(null)
const recoveryBudgetGiB = ref(0)
const revoking = ref<ExecutionNode | null>(null)
const grantingId = ref<string | null>(null)
// The page keeps polling: grants made meanwhile, as from an agent page in another window, show up
// in the "Who can use it?" step and are kept.
const granting = computed({
  get: () => nodes.value.find(node => node.id === grantingId.value) ?? null,
  set: (node: ExecutionNode | null) => {
    grantingId.value = node?.id ?? null
  },
})
const cleaning = ref<ExecutionNode | null>(null)
const granted = ref<string[]>([])
const showRevoked = ref(false)
const active = computed(() => nodes.value.filter(node => !node.revoked))
const revoked = computed(() => nodes.value.filter(node => node.revoked))
const visible = computed(() => showRevoked.value ? nodes.value : active.value)
let timer: ReturnType<typeof setInterval> | undefined
// The newly enrolled native node whose "Who can use it?" step follows its configuration.
let grantAfterConfigure: string | null = null

async function load() {
  try {
    nodes.value = await api<ExecutionNode[]>('/nodes')
    if (nodes.value.some(node => node.capabilities.backend === 'native'))
      nativeCallers.value = await api('/nodes/native-callers')
  }
  catch (e) {
    error.value = (e as Error).message
    return
  }

  if (!enrollment.value)
    return
  const invitation = enrollment.value
  // Older installations cannot safely associate a guided switch with its replacement.
  if (replacing.value && !invitation.nodeId)
    return
  const connected = nodes.value.find(node => !node.local && !node.revoked
    && nodeBackend(node) === (invitation.backend ?? 'firecracker')
    && (invitation.nodeId ? node.id === invitation.nodeId : !knownBeforeEnrollment.has(node.id)))
  if (connected) {
    enrollment.value = null
    adding.value = false
    notify(`${connected.name} is connected`)
    if (replacing.value) {
      replacement = { previous: replacing.value.id, next: connected.id }
      replacing.value = null
    }

    // A native machine is configured first (trusted people), then the same add-only step follows.
    if (connected.capabilities.backend === 'native') {
      grantAfterConfigure = connected.id
      edit(connected)
    }
    else {
      grant(connected)
    }
  }
}

onMounted(() => {
  void load()
  timer = setInterval(load, 10_000)
})
onBeforeUnmount(() => clearInterval(timer))

async function action(operation: () => Promise<void>) {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    await operation()
    await load()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

function enroll() {
  return action(async () => {
    knownBeforeEnrollment = new Set(nodes.value.map(node => node.id))
    enrollment.value = await api<NodeEnrollment>('/nodes/enrollments', {
      method: 'POST',
      body: JSON.stringify({ name: name.value, managerUrl: managerUrl.value.trim(), backend: enrollmentBackend.value }),
    })
  })
}

function addMachine() {
  replacing.value = null
  adding.value = true
}

// Another runtime enrolls as a new node, so its grants and checkpoints never mix with the old ones.
function switchBackend(node: ExecutionNode) {
  replacing.value = node
  enrollmentBackend.value = nodeBackend(node) === 'native' ? 'firecracker' : 'native'
  name.value = node.name
  enrollment.value = null
  error.value = ''
  adding.value = true
}

// The last step of a guided switch, once the replacement's setup is done or skipped.
function offerRetirement(node: string) {
  if (replacement?.next !== node)
    return
  const previous = nodes.value.find(item => item.id === replacement!.previous && !item.revoked)
  const next = nodes.value.find(item => item.id === node)
  replacement = null
  if (previous && next)
    retiring.value = { previous, next }
}

function retire() {
  const pair = retiring.value
  if (!pair)
    return
  return action(async () => {
    const result = await api<{ releasedPlacements?: number }>(`/nodes/${pair.previous.id}/revoke`, { method: 'POST', body: JSON.stringify({ releasePlacements: true }) })
    retiring.value = null
    const released = result.releasedPlacements ?? 0
    notify(released ? `${pair.previous.name} revoked; ${released} ${released === 1 ? 'conversation returns' : 'conversations return'} to automatic placement` : `${pair.previous.name} revoked`)
  })
}

function edit(node: ExecutionNode) {
  editing.value = JSON.parse(JSON.stringify(node))
  if (editing.value && node.capabilities.backend === 'native') {
    editing.value.nativeEnabled ??= false
    editing.value.nativeAccounts ??= []
  }

  tags.value = node.tags.join(', ')
  memoryGiB.value = +(node.limits.memoryMiB / 1024).toFixed(2)
  diskGiB.value = +(node.limits.diskMiB / 1024).toFixed(2)
}

function save() {
  const node = editing.value
  if (!node)
    return
  const limits = { cpu: node.limits.cpu, memoryMiB: Math.round(memoryGiB.value * 1024), diskMiB: Math.round(diskGiB.value * 1024) }
  return action(async () => {
    await api(`/nodes/${node.id}`, {
      method: 'PUT',
      body: JSON.stringify({
        name: node.name,
        tags: tags.value.split(',').map(tag => tag.trim()).filter(Boolean),
        limits,
        slots: node.slots,
        // Exclusive reservations are granted on each agent's page; omitting them keeps them.
        accepting: node.accepting,
        ...(node.capabilities.backend === 'native' ? { nativeEnabled: node.nativeEnabled, nativeAccounts: node.nativeAccounts } : {}),
      }),
    })
    editing.value = null
    notify('Node saved')
    if (grantAfterConfigure === node.id) {
      grantAfterConfigure = null
      grant(node)
    }
  })
}

function closeEditor() {
  const node = editing.value
  editing.value = null
  const pending = grantAfterConfigure
  grantAfterConfigure = null
  if (node && pending === node.id)
    offerRetirement(node.id)
}

function closeGrants() {
  const node = granting.value
  granting.value = null
  if (node)
    offerRetirement(node.id)
}

// The last step of adding a machine: nothing is granted unless the owner picks agents here.
function grant(node: ExecutionNode) {
  granting.value = node
  granted.value = []
}

function releaseExclusive(node: ExecutionNode) {
  const reservation = node.exclusiveReservation
  if (!reservation)
    return
  return action(async () => {
    await api(`/nodes/${node.id}/exclusive/${reservation.id}/release`, { method: 'POST', body: '{}' })
    notify('Exclusive release requested; suspended work resumes after confirmation')
  })
}

function saveGrants() {
  const node = granting.value
  if (!node)
    return
  // `add` only grants the chosen agents: finishing never revokes one allowed meanwhile.
  const agentIds = granted.value.filter(agentId => !allowed(agentId) && !mismatch(agentId))
  return action(async () => {
    if (agentIds.length) {
      try {
        await api(`/nodes/${node.id}/agents`, { method: 'PUT', body: JSON.stringify({ agentIds, add: true }) })
      }
      catch (e) {
        // An older installation only replaces the list: send the agents already allowed with the new ones.
        if (!(e instanceof ApiError && e.status === 400 && (e.message === 'Invalid node request.' || /unknown field/i.test(e.message))))
          throw e
        const kept = (node.agents ?? []).filter(agent => !agent.allNodes).map(agent => agent.id)
        await api(`/nodes/${node.id}/agents`, { method: 'PUT', body: JSON.stringify({ agentIds: [...new Set([...kept, ...agentIds])] }) })
      }
    }

    granting.value = null
    notify(`${node.name} is ready`)
    await refresh()
    offerRetirement(node.id)
  })
}

function freeStaleDisks() {
  const node = cleaning.value
  if (!node)
    return
  return action(async () => {
    const result = await api<{ freedMiB: number, failed: number }>(`/nodes/${node.id}/stale-disks/delete`, { method: 'POST', body: '{}' })
    cleaning.value = null
    notify(result.failed ? `Freed ${formatMiB(result.freedMiB)}; ${result.failed} disks are in use or the node is unreachable` : `Freed ${formatMiB(result.freedMiB)}`)
  })
}

/** Why an agent cannot run on the node being granted: the server only accepts agents of its backend. */
function mismatch(agentId: string) {
  const agent = state.agents.find(item => item.id === agentId)
  const native = granting.value?.capabilities.backend === 'native'
  if (!agent || (agent.access?.backend === 'native') === native)
    return ''
  return native ? 'Runs in a Firecracker VM: set it to native execution on its page first' : 'Uses native execution: only native nodes can run it'
}

function allNodes(agentId: string) {
  return (!mismatch(agentId) && state.agents.some(agent => agent.id === agentId && agent.access?.nodes === null))
    || (granting.value?.agents?.some(agent => agent.id === agentId && agent.allNodes) ?? false)
}

/** Already allowed on the node being granted, from its latest data, or on every node. */
function allowed(agentId: string) {
  return allNodes(agentId) || (granting.value?.agents?.some(agent => agent.id === agentId) ?? false)
}

function agentNames(ids: string[]) {
  return ids.map(id => state.agents.find(agent => agent.id === id)?.name ?? 'Removed agent').join(', ')
}

function configureRecovery() {
  return action(async () => {
    recovery.value = await api<NodeSyncSettings>('/nodes/settings')
    recoveryBudgetGiB.value = +(recovery.value.budgetMiB / 1024).toFixed(2)
  })
}

function saveRecovery() {
  const settings = recovery.value
  if (!settings)
    return
  return action(async () => {
    const { s3Configured: _, ...value } = settings
    await api('/nodes/settings', { method: 'PUT', body: JSON.stringify({ ...value, budgetMiB: Math.round(recoveryBudgetGiB.value * 1024) }) })
    recovery.value = null
  })
}

function statusLabel(node: ExecutionNode) {
  return node.status === 'local' ? 'master runner' : node.status
}
</script>

<template>
  <div class="mx-auto max-w-240 px-8 py-8 phone:px-4">
    <header class="nodes-heading">
      <div>
        <p class="eyebrow">
          Infrastructure
        </p><h1>Nodes</h1><p class="mt-2 text-muted">
          A home for your agents’ work. Manage machines, capacity and access.
        </p>
      </div><UiButton @click="addMachine">
        <Icon :name="Plus" :size="16" /> Add a machine
      </UiButton>
    </header>
    <div class="fleet-summary">
      <span class="inline-flex gap-1"><strong>{{ active.length }}</strong><span>{{ active.length === 1 ? 'machine' : 'machines' }}</span></span><span class="inline-flex gap-1"><strong>{{ active.filter(n => n.local || n.status === 'online').length }}</strong><span>connected</span></span><span>Linux · managed executions</span>
    </div>
    <UiAlert v-if="error && !adding" class="mt-4">
      {{ error }}
    </UiAlert>
    <Modal
      v-if="adding"
      sheet
      :title="replacing ? `Switch ${replacing.name} to ${backendNames[enrollmentBackend]}` : 'Add a machine'"
      @close="adding = false"
    >
      <section class="node-form">
        <p v-if="replacing" class="text-sm text-muted">
          Install the {{ backendNames[enrollmentBackend] }} runtime on the machine running {{ replacing.name }}. It enrolls as a new node; once it connects and is set up, Cairn offers to revoke {{ replacing.name }}. Conversations keep their execution backend, and each agent’s backend is chosen on its page.
        </p>
        <p v-else class="text-sm text-muted">
          Connect a trusted Linux machine, then choose its execution backend and authorized agents.
        </p>
        <p class="text-sm text-muted">
          The node must reach the manager directly over your local network, VPN or an optional public URL. It does not use the relay. Disk blocks never pass through the relay.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <form class="mt-3 grid gap-3" @submit.prevent="enroll">
          <VirtualSelect
            v-if="!replacing"
            v-model="enrollmentBackend"
            label="Execution backend"
            :options="[
              { value: 'firecracker', label: 'Firecracker VM' },
              { value: 'native', label: 'Native process · trusted people only' },
            ]"
          />
          <p v-if="enrollmentBackend === 'native'" class="text-sm text-muted">
            Shares the host GPU through its installed drivers. Uses weaker isolation than a VM; heavy work can slow your desktop. Requires Linux x86-64 with systemd and the ublk kernel driver.
          </p>
          <label>Machine name<input
            v-model="name"
            class="mt-1 block"
            maxlength="100"
            required
          ></label>
          <label>Direct manager address<input
            v-model="managerUrl"
            type="url"
            placeholder="https://manager.vpn.example:4310"
            aria-describedby="node-manager-address-help"
            required
          ></label>
          <p id="node-manager-address-help" class="text-sm text-muted">
            Use HTTPS with a certificate trusted by the node. HTTP is allowed only on loopback for local tests. This is the manager’s address, not the beacon app’s address.
          </p>
          <UiButton type="submit" :disabled="busy">
            Create enrollment code
          </UiButton>
        </form>
        <div v-if="enrollment" class="mt-4 grid gap-2">
          <template v-if="enrollment.installCommand">
            <p><strong>1.</strong> On the Linux machine ({{ enrollment.backend === 'native' ? 'x86-64 with Python 3, curl, systemd, e2fsprogs and the ublk_drv kernel module' : 'x86-64 with KVM, Docker, curl and systemd' }}), run:</p>
            <code class="block break-all select-all" aria-label="Node installation command">{{ enrollment.installCommand }}</code>
            <p><strong>2.</strong> When prompted, enter this single-use code. It expires {{ new Date(enrollment.expiresAt).toLocaleString() }}.</p>
          </template>
          <template v-else>
            <p>Assisted installation is unavailable: the manager has no pinned node image. Set <code>CAIRN_NODE_IMAGE</code> on the manager to the deployed image with its digest (<code>image@sha256:…</code>) and create a new code.</p>
            <p>Meanwhile, on a machine with the matching <code>cairn</code> binary, run:</p>
            <code v-if="enrollmentCommand" class="block break-all select-all" aria-label="Node enrollment command">{{ enrollmentCommand }}</code>
            <p>Paste this single-use code, press Enter, then Ctrl+D to finish standard input. It expires {{ new Date(enrollment.expiresAt).toLocaleString() }}:</p>
          </template>
          <code class="block break-all select-all" aria-label="Single-use enrollment code">{{ enrollment.code }}</code>
          <UiAlert v-if="replacing && !enrollment.nodeId">
            Update this installation to track the replacement and finish the guided switch. The previous node is kept.
          </UiAlert>
          <p class="text-sm text-muted">
            <strong>3.</strong> {{ enrollment.backend === 'native' ? 'This page detects the machine once it connects, opens its configuration to enable native execution and choose trusted people, then asks which agents may use it.' : 'This page detects the machine once it connects and asks which agents may use it, as the last step.' }}
          </p>
          <div>
            <UiButton variant="default" @click="enrollment = null">
              Hide code
            </UiButton>
          </div>
        </div>
      </section>
    </Modal>
    <div class="grid gap-4">
      <article v-for="node in visible" :key="node.id" class="node-card">
        <div class="flex items-center justify-between gap-4">
          <div class="flex min-w-0 items-center gap-3">
            <span class="node-symbol"><Icon :name="Server" :size="20" /></span><h2 class="break-words">
              {{ node.name }}
            </h2>
          </div><span class="node-status" :class="{ connected: !node.revoked && (node.local || node.status === 'online') }">{{ statusLabel(node) }}</span>
        </div>
        <p class="mt-2 text-sm text-muted">
          {{ node.capabilities.backend === 'native' ? 'Native process · shared host GPU · trusted people only' : 'Firecracker VM' }}
        </p>
        <template v-if="!node.revoked">
          <div class="capacity-grid" aria-label="Shared capacity">
            <div><span>Slots available</span><strong>{{ node.availableSlots ?? node.slots }} <small>of {{ node.slots }}</small></strong><span>{{ node.occupiedSlots ?? 0 }} active or reserved</span></div>
            <div><span>Shared CPU</span><strong>{{ node.limits.cpu }} <small>cores</small></strong><span>shared by all slots</span></div>
            <div><span>Shared RAM</span><strong>{{ formatMiB(node.limits.memoryMiB) }}</strong><span>{{ node.usage ? `${formatMiB(node.usage.memoryMiB)} in use` : 'usage unavailable' }}</span></div>
            <div><span>Shared disk</span><strong>{{ formatMiB(node.limits.diskMiB) }}</strong><span>{{ node.usage ? `${formatMiB(node.usage.diskMiB)} on this node` : 'usage unavailable' }}</span></div>
          </div>
          <section class="mt-2 grid gap-1 rounded-xl border border-line px-4 py-3 text-sm" :aria-label="`Agents on ${node.name}`">
            <p>
              <template v-if="node.agents?.length">
                {{ node.agents.length }} {{ node.agents.length === 1 ? 'agent can' : 'agents can' }} run here: <span class="text-muted">{{ node.agents.map(agent => agent.allNodes ? `${agent.name} (all nodes)` : agent.name).join(', ') }}</span>
              </template>
              <template v-else>
                No agent can run here yet.
              </template>
            </p>
            <p v-if="node.exclusiveAgents?.length">
              {{ node.exclusiveAgents.length }} can reserve it exclusively: <span class="text-muted">{{ agentNames(node.exclusiveAgents) }}</span>
            </p>
            <p class="text-xs text-muted">
              Access is set on each agent’s page. <RouterLink to="/agents" class="text-accent hover:underline">
                Manage access from the agents ›
              </RouterLink>
            </p>
          </section>
          <div v-if="node.exclusiveReservation" role="status" class="mt-3 rounded-lg border border-line p-3 text-sm">
            <p><strong>Exclusive reservation · {{ node.exclusiveReservation.state }}</strong></p>
            <p>{{ node.exclusiveReservation.reason }}</p>
            <p>{{ node.exclusiveReservation.pausedRunIds.length }} conversations suspended<span v-if="node.exclusiveReservation.expiresAt"> · until {{ new Date(node.exclusiveReservation.expiresAt).toLocaleTimeString() }}</span></p>
            <RouterLink :to="`/runs/${node.exclusiveReservation.ownerRunId}`">
              View reservation owner
            </RouterLink>
            <UiButton
              class="ml-2"
              size="small"
              variant="default"
              :disabled="busy || node.exclusiveReservation.state === 'releasing'"
              @click="releaseExclusive(node)"
            >
              Release reservation
            </UiButton>
          </div>
          <p v-if="node.staleDisks?.count" class="mt-2 text-sm">
            Old disks: {{ formatMiB(node.staleDisks.diskMiB) }} kept from {{ node.staleDisks.count }} conversation{{ node.staleDisks.count > 1 ? 's' : '' }}
            <UiButton
              class="ml-2"
              size="small"
              variant="default"
              @click="cleaning = node"
            >
              Free old disks
            </UiButton>
          </p>
          <p v-if="node.maintenance" role="status" class="mt-2">
            {{ node.maintenance === 'draining' ? 'Pausing and saving conversations for an update' : 'Ready to restart for the update' }}
          </p>
          <p v-if="node.maintenance && node.maintenanceError" role="alert" class="mt-1 text-coral">
            {{ node.maintenanceError }}
          </p>
          <ul v-if="nodeDiagnostics(node).length" class="mt-2 list-disc pl-5 text-sm text-coral" :aria-label="`Why ${node.name} cannot take new work`">
            <li v-for="reason in nodeDiagnostics(node)" :key="reason">
              {{ reason }}
            </li>
          </ul>
          <p v-if="node.tags.length || node.systemTags?.length" class="mt-2 text-sm">
            <template v-if="node.tags.length">
              {{ node.tags.join(' · ') }}
            </template>
            <span v-if="node.systemTags?.length" class="text-muted"> Detected tags: {{ node.systemTags.join(' · ') }}</span>
          </p>
          <details class="mt-2 text-xs text-muted">
            <summary>Technical details</summary>
            <p class="mt-2">
              Detected {{ node.capabilities.cpu }} CPU · {{ formatMiB(node.capabilities.memoryMiB) }} RAM · {{ formatMiB(node.capabilities.diskMiB) }} disk · {{ node.capabilities.os }} {{ node.capabilities.arch }} · {{ node.capabilities.backend === 'native' ? `ublk ${node.capabilities.ublk ? 'available' : 'unavailable'}` : `KVM ${node.capabilities.kvm ? 'available' : 'unavailable'}` }}
            </p>
            <p v-if="node.lastSeen">
              Last contact {{ new Date(node.lastSeen).toLocaleString() }}
            </p>
            <p v-if="node.imageDigest" class="break-all">
              Version: {{ node.imageDigest }}
            </p>
            <p v-if="node.runtimeId" class="break-all">
              Runtime: {{ node.runtimeId }}
            </p>
          </details>
          <div class="node-actions">
            <UiButton variant="default" @click="edit(node)">
              <Icon :name="Settings2" :size="15" /> Configure
            </UiButton>
            <NodeStorage :node="node" @saved="load" />
            <UiButton v-if="!node.local" variant="default" @click="switchBackend(node)">
              {{ nodeBackend(node) === 'native' ? 'Switch to VM' : 'Switch to native' }}
            </UiButton>
            <UiButton v-if="!node.local" variant="default" @click="revoking = node">
              Revoke
            </UiButton>
          </div>
        </template>
        <p v-else class="mt-2 text-sm text-muted">
          This machine no longer has access. Register it again with a new code to reconnect it.
        </p>
      </article>
    </div>
    <UiButton
      v-if="revoked.length"
      class="mt-4"
      size="small"
      variant="default"
      @click="showRevoked = !showRevoked"
    >
      {{ showRevoked ? 'Hide revoked machines' : `Show revoked machines (${revoked.length})` }}
    </UiButton>
    <details class="mt-6 rounded-xl border border-line bg-surface p-5">
      <summary>Advanced: S3 synchronization and timeouts</summary>
      <p class="mt-2 text-sm text-muted">
        How often disk changes are published to S3 and how long nodes wait before pausing. The defaults suit most setups.
      </p>
      <UiButton class="mt-3" variant="default" @click="configureRecovery">
        Configure synchronization
      </UiButton>
    </details>
    <Modal
      v-if="recovery"
      sheet
      title="S3 synchronization"
      @close="recovery = null"
    >
      <form class="node-form" @submit.prevent="saveRecovery">
        <p>Changed disk blocks upload in the background after a coherent capture. Chat history is streamed separately. Each conversation shows its last completed synchronization.</p>
        <p v-if="!recovery.s3Configured" class="text-sm text-muted">
          Configure S3 on the server to publish disk changes.
        </p>
        <fieldset class="form-section">
          <legend>Timing &amp; capacity</legend><div class="form-grid">
            <label>Synchronization target (seconds)<input
              v-model.number="recovery.intervalSeconds"
              class="mt-1 block"
              type="number"
              min="5"
              max="3600"
              required
            ></label>
            <label>Pause after disconnection (seconds)<input
              v-model.number="recovery.disconnectTimeoutSeconds"
              class="mt-1 block"
              type="number"
              min="10"
              max="300"
              required
            ></label>
            <label>Shutdown preparation limit (seconds)<input
              v-model.number="recovery.shutdownTimeoutSeconds"
              class="mt-1 block"
              type="number"
              min="30"
              max="300"
              required
            ></label>
            <label>Maximum capacity wait (seconds)<input
              v-model.number="recovery.maxCapacityWaitSeconds"
              class="mt-1 block"
              type="number"
              min="0"
              max="3600"
              required
            ></label>
            <label>Publication cache budget (GiB)<input
              v-model.number="recoveryBudgetGiB"
              class="mt-1 block"
              type="number"
              min="0.125"
              max="1024"
              step="any"
              required
            ></label>
          </div>
        </fieldset><p class="text-sm text-muted">
          Synchronization can take longer than the target interval. The publication cache stays within this budget.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton type="submit" :disabled="busy">
          Save
        </UiButton>
      </form>
    </Modal>
    <Modal
      v-if="granting"
      sheet
      :title="`Who can use “${granting.name}”?`"
      @close="closeGrants"
    >
      <form class="node-form" @submit.prevent="saveGrants">
        <p class="text-sm text-muted">
          Chosen agents may run their conversations on this machine. A tag or capability never grants access by itself.
        </p>
        <label v-for="agent in state.agents" :key="agent.id" class="access-row">
          <input
            v-if="mismatch(agent.id)"
            type="checkbox"
            disabled
          >
          <input
            v-else-if="allowed(agent.id)"
            type="checkbox"
            checked
            disabled
          >
          <input
            v-else
            v-model="granted"
            type="checkbox"
            :value="agent.id"
          >
          <span class="min-w-0">{{ agent.name }}<span v-if="mismatch(agent.id)" class="block text-xs text-muted">{{ mismatch(agent.id) }}</span><span v-else-if="allNodes(agent.id)" class="block text-xs text-muted">Allowed on all nodes</span><span v-else-if="allowed(agent.id)" class="block text-xs text-muted">Already allowed on this machine</span></span>
        </label>
        <p class="text-xs text-muted">
          You can change this later on each agent’s page.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <div class="flex justify-end gap-2.5">
          <UiButton :disabled="busy" @click="closeGrants">
            Later
          </UiButton>
          <UiButton type="submit" variant="primary" :disabled="busy">
            Finish
          </UiButton>
        </div>
      </form>
    </Modal>
    <Modal
      v-if="cleaning"
      sheet
      :title="`Free old disks on ${cleaning.name}`"
      @close="cleaning = null"
    >
      <div class="node-form">
        <p>These disks belong to conversations that now run on another machine, or are older copies set aside when a recovery point replaced a disk.</p>
        <p>After a failover, an old disk can hold changes newer than the recovery point the conversation resumed from. Deleting it is permanent.</p>
        <p class="text-sm text-muted">
          The machine must be online. Disks of conversations that are running or moving are kept.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert><div>
          <UiButton variant="danger" :disabled="busy" @click="freeStaleDisks">
            Delete {{ formatMiB(cleaning.staleDisks?.diskMiB || 0) }}
          </UiButton>
        </div>
      </div>
    </Modal>
    <Modal
      v-if="editing"
      sheet
      title="Configure node"
      @close="closeEditor"
    >
      <form class="node-form" @submit.prevent="save">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <label>Name<input
          v-model="editing.name"
          class="mt-1 block w-full"
          maxlength="100"
          required
        ></label>
        <label>Tags, separated by commas<input v-model="tags" class="mt-1 block w-full"></label>
        <fieldset v-if="editing.capabilities.backend === 'native'" class="form-section">
          <legend>Native execution</legend>
          <p>Only selected native agents and trusted people may run here. Disabling native execution or removing a person stops affected work after its lease expires.</p>
          <label class="access-row"><input v-model="editing.nativeEnabled" type="checkbox">Enable native execution</label>
          <template v-if="nativeCallers.length">
            <p id="native-trusted-people" class="mt-4 mb-2!">
              Trusted people
            </p>
            <div class="grid gap-2" role="group" aria-labelledby="native-trusted-people">
              <label v-for="person in nativeCallers" :key="person.id" class="access-row"><input v-model="editing.nativeAccounts" type="checkbox" :value="person.id"><span class="min-w-0 break-all">{{ person.email }}</span></label>
            </div>
          </template>
          <p v-if="!nativeCallers.length" class="text-sm text-muted">
            No current people are available. Refresh the installation connection before granting access.
          </p>
        </fieldset>
        <fieldset class="form-section">
          <legend>Slots and shared budgets</legend><p>All slots share these budgets. Resources are used on demand.</p><div class="form-grid">
            <label>Execution slots<input
              v-model.number="editing.slots"
              class="mt-1 block"
              type="number"
              min="1"
              max="4096"
              required
            ></label>
            <label>Shared CPU budget<input
              v-model.number="editing.limits.cpu"
              class="mt-1 block"
              type="number"
              min="1"
              :max="editing.capabilities.cpu"
              required
            ></label>
            <label>Shared RAM budget (GiB)<input
              v-model.number="memoryGiB"
              class="mt-1 block"
              type="number"
              min="0.125"
              step="any"
              :max="editing.capabilities.memoryMiB / 1024"
              required
            ></label>
            <label>Shared disk budget (GiB)<input
              v-model.number="diskGiB"
              class="mt-1 block"
              type="number"
              min="0.125"
              step="any"
              :max="diskCapacity(editing) / 1024"
              required
            ></label>
          </div>
        </fieldset><p class="text-sm text-muted">
          Detected on this machine: {{ editing.capabilities.cpu }} CPU · {{ formatMiB(editing.capabilities.memoryMiB) }} RAM · {{ formatMiB(diskCapacity(editing)) }} disk ({{ formatMiB(editing.capabilities.diskMiB) }} free).
        </p>
        <label class="access-row"><input v-model="editing.accepting" type="checkbox">Accept new work</label>
        <section class="form-section grid gap-1.5 text-sm" aria-label="Agents">
          <h3 class="text-[13px]! font-semibold">
            Agents <span class="text-xs font-normal text-muted">read-only</span>
          </h3>
          <p>{{ editing.agents?.length ? `${editing.agents.length} ${editing.agents.length === 1 ? 'agent can' : 'agents can'} run here: ${editing.agents.map(agent => agent.name).join(', ')}` : 'No agent can run here yet.' }}</p>
          <p v-if="editing.exclusiveAgents?.length">
            {{ editing.exclusiveAgents.length }} can reserve it exclusively: {{ agentNames(editing.exclusiveAgents) }}
          </p>
          <p v-if="!editing.exclusiveReservations && editing.capabilities.backend !== 'native'">
            This runtime needs an update before exclusive reservations can be acquired.
          </p>
          <p class="text-xs">
            Access is set on each agent’s page. <RouterLink to="/agents" class="text-accent hover:underline">
              Manage access from the agents ›
            </RouterLink>
          </p>
        </section>
        <UiButton type="submit" :disabled="busy">
          Save
        </UiButton>
      </form>
    </Modal>
    <Modal
      v-if="revoking"
      sheet
      title="Revoke node"
      @close="revoking = null"
    >
      <div class="node-form">
        <p>{{ revoking.name }} will immediately lose access to the master, and agents lose their permission to use it.</p>
        <p v-if="revoking.occupiedSlots">
          Conversations running there pause within the disconnection delay, then resume from their latest recovery point on another authorized machine when one has capacity. Conversations fixed to this machine, or without a recovery point, wait.
        </p>
        <p>Files stored on the machine are not deleted. To uninstall it, run on the machine:</p>
        <code class="block break-all select-all">{{ uninstallCommands[nodeBackend(revoking)] }}</code>
        <p class="text-sm text-muted">
          Removing these directories also deletes the local conversation disks kept on that machine.
        </p>
        <p v-if="revoking.capabilities.backend === 'native'" class="text-sm text-muted">
          If a conversation disk is still mounted, the command lists it and stops before deleting anything: start cairn-native-node so its recovery releases owned disks, then run the command again.
        </p>
        <div>
          <UiButton variant="danger" :disabled="busy" @click="action(async () => { await api(`/nodes/${revoking!.id}/revoke`, { method: 'POST', body: '{}' }); revoking = null })">
            Revoke node
          </UiButton>
        </div>
      </div>
    </Modal>
    <Modal
      v-if="retiring"
      sheet
      :title="`Retire the previous ${backendNames[nodeBackend(retiring.previous)]} node?`"
      @close="retiring = null"
    >
      <div class="node-form">
        <p>{{ retiring.next.name }} now runs as a {{ backendNames[nodeBackend(retiring.next)] }} node. Revoke the previous {{ backendNames[nodeBackend(retiring.previous)] }} node, {{ retiring.previous.name }}, to finish the switch: it loses access to the master, and agents lose their permission to use it.</p>
        <p v-if="retiring.previous.agents?.some(agent => !agent.allNodes)">
          Agents allowed on the previous node: {{ retiring.previous.agents.filter(agent => !agent.allNodes).map(agent => agent.name).join(', ') }}. To run them on the new node, set them to {{ backendNames[nodeBackend(retiring.next)] }} on their page and allow it.
          <RouterLink to="/agents" class="text-accent hover:underline">
            Open agents ›
          </RouterLink>
        </p>
        <p>Conversations fixed to or preferring the previous node return to automatic placement. Conversations keep their execution backend: existing ones continue on other {{ backendNames[nodeBackend(retiring.previous)] }} nodes.</p>
        <p v-if="retiring.previous.occupiedSlots">
          Conversations running there pause within the disconnection delay, then resume from their latest recovery point on another authorized machine when one has capacity.
        </p>
        <p>Then uninstall the previous runtime on the machine:</p>
        <code class="block break-all select-all" aria-label="Previous runtime uninstall command">{{ uninstallCommands[nodeBackend(retiring.previous)] }}</code>
        <p class="text-sm text-muted">
          Removing these directories also deletes the local conversation disks kept for the previous node.
        </p>
        <p v-if="nodeBackend(retiring.previous) === 'native'" class="text-sm text-muted">
          If a conversation disk is still mounted, the command lists it and stops before deleting anything: start cairn-native-node so its recovery releases owned disks, then run the command again.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <div class="flex flex-wrap justify-end gap-2.5">
          <UiButton :disabled="busy" @click="retiring = null">
            Keep both for now
          </UiButton>
          <UiButton variant="danger" :disabled="busy" @click="retire">
            Revoke previous node
          </UiButton>
        </div>
      </div>
    </Modal>
  </div>
</template>

<style scoped>
.nodes-heading { display: flex; align-items: center; justify-content: space-between; gap: 24px; }
.eyebrow { text-transform: uppercase; letter-spacing: .12em; font-size: 10px; color: var(--color-muted); margin-bottom: 6px; }
.fleet-summary { display: flex; flex-wrap: wrap; gap: 12px 24px; padding: 24px 0; color: var(--color-muted); font-size: 12px; }
.fleet-summary strong { color: var(--color-ink); }
.node-card { border: 1px solid var(--color-line); border-radius: 18px; background: var(--color-surface); padding: 24px; }
.node-symbol { display: grid; place-items: center; padding: 10px; border-radius: 12px; background: var(--color-inset); color: var(--color-accent); }
.node-status { border: 1px solid var(--color-line); border-radius: 100px; padding: 4px 10px; font-size: 11px; white-space: nowrap; color: var(--color-muted); }
.node-status.connected { color: var(--color-accent); background: var(--color-inset); }
.capacity-grid { display: grid; grid-template-columns: repeat(4,minmax(0,1fr)); gap: 1px; margin: 22px 0; border: 1px solid var(--color-line); border-radius: 12px; overflow: hidden; background: var(--color-line); }
.capacity-grid > div { display: grid; gap: 5px; background: var(--color-inset); padding: 14px; }
.capacity-grid span { font-size: 11px; color: var(--color-muted); }
.capacity-grid strong { font-size: 19px; font-weight: 600; }
.capacity-grid small { font-size: 11px; font-weight: 400; color: var(--color-muted); }
.node-actions { display: flex; flex-wrap: wrap; gap: 8px; border-top: 1px solid var(--color-line); padding-top: 16px; margin-top: 18px; }
.node-form { display: grid; gap: 20px; padding: 24px; }
.node-form label:not(.access-row) { display: grid; gap: 6px; font-size: 12px; color: var(--color-muted); }
.node-form input:not([type=checkbox]) { width: 100%; min-width: 0; color: var(--color-ink); }
.node-form code { border: 1px solid var(--color-line); padding: 14px; background: var(--color-inset); border-radius: 10px; font-size: 12px; }
.form-section { border-top: 1px solid var(--color-line); padding-top: 16px; }
.form-section legend { font-size: 13px; font-weight: 600; padding-right: 10px; }
.form-section p { color: var(--color-muted); font-size: 12px; margin-bottom: 12px; }
.form-grid { display: grid; grid-template-columns: repeat(2,minmax(0,1fr)); gap: 16px; }
.access-row { display: flex; flex-direction: row; align-items: center; gap: 12px; border: 1px solid var(--color-line); background: var(--color-inset); border-radius: 10px; padding: 14px; font-size: 13px; color: var(--color-ink); }
.access-row input { width: 16px; height: 16px; flex-shrink: 0; margin: 0; }
@media (max-width: 640px) { .nodes-heading { align-items: start; flex-direction: column; gap: 16px; } .node-card { padding: 16px; } .capacity-grid { grid-template-columns: repeat(2,minmax(0,1fr)); } .capacity-grid strong { font-size: 15px; } .capacity-grid > div { padding: 10px; } .node-form { padding: 20px; } .form-grid { grid-template-columns: 1fr; } }
</style>
