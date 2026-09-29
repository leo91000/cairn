<script setup lang="ts">
import type { ExecutionNode, NodeEnrollment, NodeSyncSettings } from '../../shared/nodes'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import { formatMiB, formatResources, nodeDiagnostics } from '../../shared/nodes'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import NodeStorage from '../components/NodeStorage.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  Plus,
  Server,
  Settings2,
  Shield,
} from '../icons'

const nodes = ref<ExecutionNode[]>([])
const editing = ref<ExecutionNode | null>(null)
const tags = ref('')
const name = ref('')
// Ceilings are edited in GiB and stored in MiB.
const memoryGiB = ref(0)
const diskGiB = ref(0)
const adding = ref(false)
const enrollment = ref<NodeEnrollment | null>(null)
// Machines known when the code was created, to recognise the newly connected one.
let knownBeforeEnrollment = new Set<string>()
const error = ref('')
const busy = ref(false)
const recovery = ref<NodeSyncSettings | null>(null)
const recoveryBudgetGiB = ref(0)
const revoking = ref<ExecutionNode | null>(null)
const granting = ref<ExecutionNode | null>(null)
const cleaning = ref<ExecutionNode | null>(null)
const granted = ref<string[]>([])
const showRevoked = ref(false)
const active = computed(() => nodes.value.filter(node => !node.revoked))
const revoked = computed(() => nodes.value.filter(node => node.revoked))
const visible = computed(() => showRevoked.value ? nodes.value : active.value)
let timer: ReturnType<typeof setInterval> | undefined

async function load() {
  try {
    nodes.value = await api<ExecutionNode[]>('/nodes')
  }
  catch (e) {
    error.value = (e as Error).message
    return
  }

  if (!enrollment.value)
    return
  const connected = nodes.value.find(node => !node.local && !node.revoked && !knownBeforeEnrollment.has(node.id))
  if (connected) {
    enrollment.value = null
    adding.value = false
    notify(`${connected.name} is connected`)
    grant(connected)
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
    enrollment.value = await api<NodeEnrollment>('/nodes/enrollments', { method: 'POST', body: JSON.stringify({ name: name.value }) })
  })
}

function edit(node: ExecutionNode) {
  editing.value = JSON.parse(JSON.stringify(node))
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
        accepting: node.accepting,
      }),
    })
    editing.value = null
    notify('Node saved')
  })
}

function grant(node: ExecutionNode) {
  granting.value = node
  granted.value = (node.agents || []).filter(agent => !agent.allNodes).map(agent => agent.id)
}

function saveGrants() {
  const node = granting.value
  if (!node)
    return
  return action(async () => {
    await api(`/nodes/${node.id}/agents`, { method: 'PUT', body: JSON.stringify({ agentIds: granted.value }) })
    granting.value = null
    notify('Agent access saved')
    await refresh()
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

function allNodes(agentId: string) {
  return granting.value?.agents?.some(agent => agent.id === agentId && agent.allNodes) ?? false
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
      </div><UiButton @click="adding = true">
        <Icon :name="Plus" :size="16" /> Add a machine
      </UiButton>
    </header>
    <div class="fleet-summary">
      <span class="inline-flex gap-1"><strong>{{ active.length }}</strong><span>{{ active.length === 1 ? 'machine' : 'machines' }}</span></span><span class="inline-flex gap-1"><strong>{{ active.filter(n => n.local || n.status === 'online').length }}</strong><span>connected</span></span><span>Linux · isolated conversations</span>
    </div>
    <UiAlert v-if="error" class="mt-4">
      {{ error }}
    </UiAlert>
    <Modal
      v-if="adding"
      sheet
      title="Add a machine"
      @close="adding = false"
    >
      <section class="node-form">
        <p class="text-sm text-muted">
          Connect a trusted Linux machine, then choose which agents can use it. GPU execution is not available yet.
        </p>
        <form class="mt-3 flex flex-wrap items-end gap-3" @submit.prevent="enroll">
          <label>Machine name<input
            v-model="name"
            class="mt-1 block"
            maxlength="100"
            required
          ></label>
          <UiButton type="submit" :disabled="busy">
            Create enrollment code
          </UiButton>
        </form>
        <div v-if="enrollment" class="mt-4 grid gap-2">
          <template v-if="enrollment.installCommand">
            <p><strong>1.</strong> On the Linux machine (x86-64 with KVM, Docker, curl and systemd), run:</p>
            <code class="block break-all select-all">{{ enrollment.installCommand }}</code>
            <p><strong>2.</strong> When prompted, enter this single-use code. It expires {{ new Date(enrollment.expiresAt).toLocaleString() }}.</p>
          </template>
          <template v-else>
            <p>Assisted installation is unavailable: the master has no pinned node image. Set <code>LEO_NODE_IMAGE</code> on the master to the deployed image with its digest (<code>image@sha256:…</code>) and create a new code.</p>
            <p>Meanwhile, you can enroll a machine that already has the matching <code>leo</code> binary with <code>leo node-enroll</code> and this single-use code, which expires {{ new Date(enrollment.expiresAt).toLocaleString() }}:</p>
          </template>
          <code class="block break-all select-all" aria-label="Single-use enrollment code">{{ enrollment.code }}</code>
          <p class="text-sm text-muted">
            <strong>3.</strong> This page detects the machine once it connects and asks which agents may use it.
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
        <template v-if="!node.revoked">
          <div class="capacity-grid" aria-label="Available capacity">
            <div><span>CPU available</span><strong>{{ (node.available || node.limits).cpu }} <small>cores</small></strong><span>of {{ node.limits.cpu }} allowed</span></div>
            <div><span>RAM available</span><strong>{{ formatMiB((node.available || node.limits).memoryMiB) }}</strong><span>of {{ formatMiB(node.limits.memoryMiB) }} allowed</span></div>
            <div><span>Disk available</span><strong>{{ formatMiB((node.available || node.limits).diskMiB) }}</strong><span>of {{ formatMiB(node.limits.diskMiB) }} allowed</span></div>
          </div>
          <p v-if="node.agents?.length" class="mt-2 text-sm">
            Used by {{ node.agents.map(agent => agent.allNodes ? `${agent.name} (all nodes)` : agent.name).join(', ') }}
          </p>
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
              Detected {{ node.capabilities.cpu }} CPU · {{ formatMiB(node.capabilities.memoryMiB) }} RAM · {{ formatMiB(node.capabilities.diskMiB) }} disk · {{ node.capabilities.os }} {{ node.capabilities.arch }} · KVM {{ node.capabilities.kvm ? 'available' : 'unavailable' }}
            </p>
            <p v-if="node.reserved">
              Reserved {{ formatResources(node.reserved) }}
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
            <UiButton :variant="node.agents?.length ? 'default' : 'primary'" @click="grant(node)">
              <Icon :name="Shield" :size="15" /> {{ node.agents?.length ? 'Agents' : 'Choose agents' }}
            </UiButton>
            <UiButton variant="default" @click="edit(node)">
              <Icon :name="Settings2" :size="15" /> Configure
            </UiButton>
            <NodeStorage :node="node" @saved="load" />
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
      :title="`Agents allowed on ${granting.name}`"
      @close="granting = null"
    >
      <form class="node-form" @submit.prevent="saveGrants">
        <p class="text-sm text-muted">
          Chosen agents may run conversations on this machine. A tag or capability never grants access by itself.
        </p>
        <label v-for="agent in state.agents" :key="agent.id" class="access-row">
          <input
            v-if="allNodes(agent.id)"
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
          {{ agent.name }}<span v-if="allNodes(agent.id)" class="text-muted">(allowed on all nodes)</span>
        </label>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton type="submit" :disabled="busy">
          Save access
        </UiButton>
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
      @close="editing = null"
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
        <fieldset class="form-section">
          <legend>Resource ceilings</legend><p>Maximum resources this machine may give to conversations.</p><div class="form-grid">
            <label>CPU ceiling<input
              v-model.number="editing.limits.cpu"
              class="mt-1 block"
              type="number"
              min="1"
              :max="editing.capabilities.cpu"
              required
            ></label>
            <label>RAM ceiling (GiB)<input
              v-model.number="memoryGiB"
              class="mt-1 block"
              type="number"
              min="0.125"
              step="any"
              :max="editing.capabilities.memoryMiB / 1024"
              required
            ></label>
            <label>Disk ceiling (GiB)<input
              v-model.number="diskGiB"
              class="mt-1 block"
              type="number"
              min="0.125"
              step="any"
              :max="editing.capabilities.diskMiB / 1024"
              required
            ></label>
          </div>
        </fieldset><p class="text-sm text-muted">
          Detected on this machine: {{ editing.capabilities.cpu }} CPU · {{ formatMiB(editing.capabilities.memoryMiB) }} RAM · {{ formatMiB(editing.capabilities.diskMiB) }} disk.
        </p>
        <label class="flex gap-2"><input v-model="editing.accepting" type="checkbox">Accept new work</label>
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
        <p v-if="revoking.reserved?.cpu">
          Conversations running there pause within the disconnection delay, then resume from their latest recovery point on another authorized machine when one has capacity. Conversations fixed to this machine, or without a recovery point, wait.
        </p>
        <p>Files stored on the machine are not deleted. To uninstall it, run on the machine:</p>
        <code class="block break-all select-all">sudo systemctl disable --now leo-node; sudo docker rm -f leo-execution-node; sudo rm -rf /etc/systemd/system/leo-node.service /opt/leo-node /var/lib/leo-node</code>
        <p class="text-sm text-muted">
          The last command also deletes the local conversation disks kept on that machine.
        </p>
        <div>
          <UiButton variant="danger" :disabled="busy" @click="action(async () => { await api(`/nodes/${revoking!.id}/revoke`, { method: 'POST', body: '{}' }); revoking = null })">
            Revoke node
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
.capacity-grid { display: grid; grid-template-columns: repeat(3,minmax(0,1fr)); gap: 1px; margin: 22px 0; border: 1px solid var(--color-line); border-radius: 12px; overflow: hidden; background: var(--color-line); }
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
.access-row { display: flex; align-items: center; gap: 12px; border: 1px solid var(--color-line); background: var(--color-inset); border-radius: 10px; padding: 14px; font-size: 13px; }
@media (max-width: 640px) { .nodes-heading { align-items: start; flex-direction: column; gap: 16px; } .node-card { padding: 16px; } .capacity-grid strong { font-size: 15px; } .capacity-grid > div { padding: 10px; } .node-form { padding: 20px; } .form-grid { grid-template-columns: 1fr; } }
</style>
