<script setup lang="ts">
import type { ExecutionNode, NodeBackupSettings, NodeEnrollment } from '../../shared/nodes'
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { api, notify } from '../api'
import Modal from '../components/Modal.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'

const nodes = ref<ExecutionNode[]>([])
const editing = ref<ExecutionNode | null>(null)
const tags = ref('')
const name = ref('')
const enrollment = ref<NodeEnrollment | null>(null)
const error = ref('')
const busy = ref(false)
const recovery = ref<NodeBackupSettings | null>(null)
const revoking = ref<ExecutionNode | null>(null)
let timer: ReturnType<typeof setInterval> | undefined
async function load() {
  try {
    nodes.value = await api<ExecutionNode[]>('/nodes')
  }
  catch (e) {
    error.value = (e as Error).message
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
function edit(node: ExecutionNode) {
  editing.value = JSON.parse(JSON.stringify(node))
  tags.value = node.tags.join(', ')
}
function save() {
  const node = editing.value
  if (!node)
    return
  return action(async () => {
    await api(`/nodes/${node.id}`, { method: 'PUT', body: JSON.stringify({ name: node.name, tags: tags.value.split(',').map(tag => tag.trim()).filter(Boolean), limits: node.limits, accepting: node.accepting }) })
    editing.value = null
    notify('Node saved')
  })
}
</script>

<template>
  <div class="mx-auto max-w-240 px-8 py-8 phone:px-4">
    <h1>Nodes</h1>
    <p class="mt-2 text-muted">
      Trusted machines and their detected capabilities. GPU execution is not available.
    </p>
    <p class="mt-2 text-sm text-muted">
      CPU conversations run in Firecracker. A node must be connected with a compatible runtime and have enough available capacity.
    </p>
    <UiAlert v-if="error" class="mt-4">
      {{ error }}
    </UiAlert>
    <form class="my-6 flex flex-wrap items-end gap-3" @submit.prevent="action(async () => { enrollment = await api<NodeEnrollment>('/nodes/enrollments', { method: 'POST', body: JSON.stringify({ name }) }) })">
      <label>Machine name<input v-model="name" class="mt-1 block" maxlength="100" required></label>
      <UiButton type="submit" :disabled="busy">
        Create enrollment code
      </UiButton>
    </form>
    <div v-if="enrollment" class="mb-6 rounded-xl border border-line p-4">
      <p>Single-use code · expires {{ new Date(enrollment.expiresAt).toLocaleString() }}</p>
      <code class="my-2 block break-all select-all">{{ enrollment.code }}</code>
      <template v-if="enrollment.installCommand">
        <p>On the trusted Linux node, run this command and enter the code when prompted:</p>
        <code class="my-2 block break-all select-all">{{ enrollment.installCommand }}</code>
      </template>
      <p v-else>
        The master needs an immutable deployed image digest before assisted installation is available.
      </p>
      <UiButton variant="default" @click="enrollment = null">
        Hide code
      </UiButton>
    </div>
    <UiButton class="mb-4" variant="default" @click="action(async () => { recovery = await api<NodeBackupSettings>('/nodes/settings') })">
      Configure recovery points
    </UiButton>
    <Modal v-if="recovery" title="Recovery points" @close="recovery = null">
      <form class="grid gap-4" @submit.prevent="action(async () => { await api('/nodes/settings', { method: 'PUT', body: JSON.stringify(recovery) }); recovery = null })">
        <p>Changed blocks upload in the background after a coherent capture. Chat streaming is separate. Actual recovery dates appear in each conversation.</p>
        <label>Destination<select v-model="recovery.destination" class="mt-1 block"><option value="master">Master storage</option><option value="s3">Configured S3 storage</option></select></label>
        <label>Capture interval (seconds)<input v-model.number="recovery.intervalSeconds" class="mt-1 block" type="number" min="5" max="3600" required></label>
        <label>Pause after disconnection (seconds)<input v-model.number="recovery.disconnectTimeoutSeconds" class="mt-1 block" type="number" min="10" max="300" required></label>
        <label>Shutdown preparation limit (seconds)<input v-model.number="recovery.shutdownTimeoutSeconds" class="mt-1 block" type="number" min="30" max="300" required></label>
        <label>Maximum capacity wait (seconds)<input v-model.number="recovery.maxCapacityWaitSeconds" class="mt-1 block" type="number" min="0" max="3600" required></label>
        <label>Recovery points to retain<input v-model.number="recovery.retention" class="mt-1 block" type="number" min="1" max="100" required></label>
        <label>Master storage budget (MiB)<input v-model.number="recovery.budgetMiB" class="mt-1 block" type="number" min="128" max="1048576" required></label>
        <p>Captures can take longer than the interval. S3 points also keep a master cache within this budget.</p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton type="submit" :disabled="busy">
          Save
        </UiButton>
      </form>
    </Modal>
    <div class="grid gap-4">
      <article v-for="node in nodes" :key="node.id" class="rounded-xl border border-line bg-surface p-5">
        <div class="flex items-center justify-between gap-4">
          <h2>{{ node.name }}</h2><span>{{ node.status }}</span>
        </div>
        <p class="mt-2 text-sm text-muted">
          {{ node.capabilities.os }} · {{ node.capabilities.arch }} · KVM {{ node.capabilities.kvm ? 'available' : 'unavailable' }}
        </p>
        <p class="mt-2 text-sm text-muted">
          Detected: {{ node.capabilities.cpu }} CPU · {{ node.capabilities.memoryMiB }} MiB RAM · {{ node.capabilities.diskMiB }} MiB disk
        </p>
        <p class="mt-2">
          {{ node.limits.cpu }} CPU · {{ node.limits.memoryMiB }} MiB RAM · {{ node.limits.diskMiB }} MiB disk allowed
        </p>
        <p v-if="node.available" class="mt-2 text-sm text-muted">
          Available: {{ node.available.cpu }} CPU · {{ node.available.memoryMiB }} MiB RAM · {{ node.available.diskMiB }} MiB disk
        </p>
        <p v-if="node.maintenance" role="status" class="mt-2">
          {{ node.maintenance }}
        </p>
        <p v-if="node.tags.length" class="mt-2 text-sm">
          {{ node.tags.join(' · ') }}
        </p>
        <p class="mt-2 text-sm text-muted">
          {{ node.accepting ? 'Admission enabled' : 'Admission paused' }}<template v-if="node.lastSeen">
            · Last contact {{ new Date(node.lastSeen).toLocaleString() }}
          </template>
        </p>
        <p v-if="node.imageDigest" class="mt-1 break-all text-xs text-muted">
          Version: {{ node.imageDigest }}
        </p>
        <p v-if="node.updateError" role="alert" class="mt-1 text-coral">
          {{ node.updateError }}
        </p>
        <p v-if="node.runtimeId" class="mt-1 break-all text-xs text-muted">
          Runtime: {{ node.runtimeId }}
        </p>
        <div v-if="!node.revoked" class="mt-4 flex gap-2">
          <UiButton variant="default" @click="edit(node)">
            Configure
          </UiButton>
          <UiButton v-if="!node.local" variant="default" @click="revoking = node">
            Revoke
          </UiButton>
        </div>
      </article>
    </div>
    <Modal v-if="editing" title="Configure node" @close="editing = null">
      <form class="grid gap-4" @submit.prevent="save">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <label>Name<input v-model="editing.name" class="mt-1 block w-full" maxlength="100" required></label>
        <label>Tags, separated by commas<input v-model="tags" class="mt-1 block w-full"></label>
        <label>CPU ceiling<input v-model.number="editing.limits.cpu" class="mt-1 block" type="number" min="1" required></label>
        <label>RAM ceiling (MiB)<input v-model.number="editing.limits.memoryMiB" class="mt-1 block" type="number" min="128" required></label>
        <label>Disk ceiling (MiB)<input v-model.number="editing.limits.diskMiB" class="mt-1 block" type="number" min="128" required></label>
        <label class="flex gap-2"><input v-model="editing.accepting" type="checkbox">Accept new work</label>
        <UiButton type="submit" :disabled="busy">
          Save
        </UiButton>
      </form>
    </Modal>
    <Modal v-if="revoking" title="Revoke node" @close="revoking = null">
      <p>{{ revoking.name }} will lose access to the master. Register it again to reconnect.</p>
      <UiButton class="mt-4" :disabled="busy" @click="action(async () => { await api(`/nodes/${revoking!.id}/revoke`, { method: 'POST', body: '{}' }); revoking = null })">
        Revoke node
      </UiButton>
    </Modal>
  </div>
</template>
