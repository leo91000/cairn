<script setup lang="ts">
import type { ExecutionNode, NodeBackupSettings, NodeEnrollment } from '../../shared/nodes'
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { formatMiB, formatResources, nodeDiagnostics } from '../../shared/nodes'
import { api, notify, refresh, state } from '../api'
import Modal from '../components/Modal.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'

const nodes = ref<ExecutionNode[]>([])
const editing = ref<ExecutionNode | null>(null)
const tags = ref('')
const name = ref('')
// Ceilings are edited in GiB and stored in MiB.
const memoryGiB = ref(0)
const diskGiB = ref(0)
const enrollment = ref<NodeEnrollment | null>(null)
// Machines known when the code was created, to recognise the newly connected one.
let knownBeforeEnrollment = new Set<string>()
const error = ref('')
const busy = ref(false)
const recovery = ref<NodeBackupSettings | null>(null)
const recoveryBudgetGiB = ref(0)
const revoking = ref<ExecutionNode | null>(null)
const granting = ref<ExecutionNode | null>(null)
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
    await api(`/nodes/${node.id}`, { method: 'PUT', body: JSON.stringify({ name: node.name, tags: tags.value.split(',').map(tag => tag.trim()).filter(Boolean), limits, accepting: node.accepting }) })
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
function allNodes(agentId: string) {
  return granting.value?.agents?.some(agent => agent.id === agentId && agent.allNodes) ?? false
}
function configureRecovery() {
  return action(async () => {
    recovery.value = await api<NodeBackupSettings>('/nodes/settings')
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
    <h1>Nodes</h1>
    <p class="mt-2 text-muted">
      Trusted Linux machines that run your agents' conversations, each in its own Firecracker VM. GPU execution is not available yet.
    </p>
    <UiAlert v-if="error" class="mt-4">
      {{ error }}
    </UiAlert>
    <section class="my-6 rounded-xl border border-line p-5">
      <h2>Add a machine</h2>
      <form class="mt-3 flex flex-wrap items-end gap-3" @submit.prevent="enroll">
        <label>Machine name<input v-model="name" class="mt-1 block" maxlength="100" required></label>
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
    <div class="grid gap-4">
      <article v-for="node in visible" :key="node.id" class="rounded-xl border border-line bg-surface p-5">
        <div class="flex items-center justify-between gap-4">
          <h2>{{ node.name }}</h2><span class="text-sm">{{ statusLabel(node) }}</span>
        </div>
        <template v-if="!node.revoked">
          <p class="mt-2">
            Available: {{ node.available ? formatResources(node.available) : formatResources(node.limits) }}
          </p>
          <p class="mt-1 text-sm text-muted">
            Allowed {{ formatResources(node.limits) }}<template v-if="node.reserved">
              · reserved {{ formatResources(node.reserved) }}
            </template>
          </p>
          <p class="mt-1 text-sm text-muted">
            Detected {{ node.capabilities.cpu }} CPU · {{ formatMiB(node.capabilities.memoryMiB) }} RAM · {{ formatMiB(node.capabilities.diskMiB) }} disk · {{ node.capabilities.os }} {{ node.capabilities.arch }} · KVM {{ node.capabilities.kvm ? 'available' : 'unavailable' }}
          </p>
          <p v-if="node.agents?.length" class="mt-2 text-sm">
            Used by {{ node.agents.map(agent => agent.allNodes ? `${agent.name} (all nodes)` : agent.name).join(', ') }}
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
          <div class="mt-4 flex flex-wrap gap-2">
            <UiButton :variant="node.agents?.length ? 'default' : 'primary'" @click="grant(node)">
              {{ node.agents?.length ? 'Agents' : 'Choose agents' }}
            </UiButton>
            <UiButton variant="default" @click="edit(node)">
              Configure
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
    <UiButton v-if="revoked.length" class="mt-4" size="small" variant="default" @click="showRevoked = !showRevoked">
      {{ showRevoked ? 'Hide revoked machines' : `Show revoked machines (${revoked.length})` }}
    </UiButton>
    <details class="mt-8 rounded-xl border border-line p-5">
      <summary>Advanced: recovery points and timeouts</summary>
      <p class="mt-2 text-sm text-muted">
        How often VM environments are saved, where recovery points are kept and how long nodes wait before pausing. The defaults suit most setups.
      </p>
      <UiButton class="mt-3" variant="default" @click="configureRecovery">
        Configure recovery points
      </UiButton>
    </details>
    <Modal v-if="recovery" title="Recovery points" @close="recovery = null">
      <form class="grid gap-4" @submit.prevent="saveRecovery">
        <p>Changed disk blocks upload in the background after a coherent capture. Chat history is streamed separately. Each conversation shows the date of its latest recovery point.</p>
        <label>Destination<select v-model="recovery.destination" class="mt-1 block"><option value="master">Master storage</option><option value="s3" :disabled="!recovery.s3Configured">S3 storage{{ recovery.s3Configured ? '' : ' (not configured)' }}</option></select></label>
        <p v-if="!recovery.s3Configured" class="text-sm text-muted">
          Configure S3 on the server, as for conversation archival, to store recovery points there.
        </p>
        <label>Capture interval (seconds)<input v-model.number="recovery.intervalSeconds" class="mt-1 block" type="number" min="5" max="3600" required></label>
        <label>Pause after disconnection (seconds)<input v-model.number="recovery.disconnectTimeoutSeconds" class="mt-1 block" type="number" min="10" max="300" required></label>
        <label>Shutdown preparation limit (seconds)<input v-model.number="recovery.shutdownTimeoutSeconds" class="mt-1 block" type="number" min="30" max="300" required></label>
        <label>Maximum capacity wait (seconds)<input v-model.number="recovery.maxCapacityWaitSeconds" class="mt-1 block" type="number" min="0" max="3600" required></label>
        <label>Recovery points to retain<input v-model.number="recovery.retention" class="mt-1 block" type="number" min="1" max="100" required></label>
        <label>Master storage budget (GiB)<input v-model.number="recoveryBudgetGiB" class="mt-1 block" type="number" min="0.125" max="1024" step="any" required></label>
        <p class="text-sm text-muted">
          Captures can take longer than the interval. S3 points also keep a master cache within this budget.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton type="submit" :disabled="busy">
          Save
        </UiButton>
      </form>
    </Modal>
    <Modal v-if="granting" :title="`Agents allowed on ${granting.name}`" @close="granting = null">
      <form class="grid gap-3" @submit.prevent="saveGrants">
        <p class="text-sm text-muted">
          Chosen agents may run conversations on this machine. A tag or capability never grants access by itself.
        </p>
        <label v-for="agent in state.agents" :key="agent.id" class="flex gap-2">
          <input v-if="allNodes(agent.id)" type="checkbox" checked disabled>
          <input v-else v-model="granted" type="checkbox" :value="agent.id">
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
    <Modal v-if="editing" title="Configure node" @close="editing = null">
      <form class="grid gap-4" @submit.prevent="save">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <label>Name<input v-model="editing.name" class="mt-1 block w-full" maxlength="100" required></label>
        <label>Tags, separated by commas<input v-model="tags" class="mt-1 block w-full"></label>
        <label>CPU ceiling<input v-model.number="editing.limits.cpu" class="mt-1 block" type="number" min="1" :max="editing.capabilities.cpu" required></label>
        <label>RAM ceiling (GiB)<input v-model.number="memoryGiB" class="mt-1 block" type="number" min="0.125" step="any" :max="editing.capabilities.memoryMiB / 1024" required></label>
        <label>Disk ceiling (GiB)<input v-model.number="diskGiB" class="mt-1 block" type="number" min="0.125" step="any" :max="editing.capabilities.diskMiB / 1024" required></label>
        <p class="text-sm text-muted">
          Detected on this machine: {{ editing.capabilities.cpu }} CPU · {{ formatMiB(editing.capabilities.memoryMiB) }} RAM · {{ formatMiB(editing.capabilities.diskMiB) }} disk.
        </p>
        <label class="flex gap-2"><input v-model="editing.accepting" type="checkbox">Accept new work</label>
        <UiButton type="submit" :disabled="busy">
          Save
        </UiButton>
      </form>
    </Modal>
    <Modal v-if="revoking" title="Revoke node" @close="revoking = null">
      <div class="grid gap-3">
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
