<script setup lang="ts">
import type { NodeConfiguration } from '../../../../packages/contracts/generated/typescript/NodeConfiguration'
import type {
  ExecutionNode,
  NodeEnrollment,
  NodeSyncSettings,
  RunnerObservation,
  UnreadyObservations,
} from '../../../../packages/contracts/nodes'
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { createNodeEnrollment } from '../../../../packages/contracts/generated/createNodeEnrollment'
import { releaseNodeReservation } from '../../../../packages/contracts/generated/releaseNodeReservation'
import { revokeNode } from '../../../../packages/contracts/generated/revokeNode'
import {
  diskCapacity,
  fleetClock,
  fleetSummary,
  formatMiB,
  observeRunner,
  observeUnready,
} from '../../../../packages/contracts/nodes'
import { nodeBackend } from '../agent-access'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import NodeDetail from '../components/NodeDetail.vue'
import NodeRow from '../components/NodeRow.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  Plus,
  Settings2,
} from '../icons'

const nodes = ref<ExecutionNode[]>([])
const nativeCallers = ref<Array<{ id: string, email: string }>>([])
const enrollmentBackend = ref<'firecracker' | 'native'>('firecracker')

// Stops before deleting while a conversation disk is mounted and keeps the unit until the rest is gone, so it can be rerun.
// Both runtimes share the gateway to Cairn; the last one on the machine removes it.
const nativeUninstallCommand = `sudo sh -c 'if [ -e /etc/systemd/system/cairn-native-node.service ]; then systemctl disable --now cairn-native-node || exit; fi; mounts=$(findmnt -rno TARGET) || { echo "findmnt could not list mounts." >&2; exit 1; }; printf "%s\\n" "$mounts" | grep ^/var/lib/cairn-native/; match=$?; if [ "$match" -ne 1 ]; then echo "Mount safety could not be confirmed. Run sudo systemctl start cairn-native-node to release owned conversation disks, then retry." >&2; exit 1; fi; cairn_policy() { grep -Fqx "# Cairn native administrator; protected units retain NoNewPrivileges." "$1" || return $?; grep -Fqx "cairn-workload ALL=(ALL:ALL) NOPASSWD: ALL" "$1"; }; for policy in /etc/sudoers.d/cairn-native-*; do [ -e "$policy" ] || [ -L "$policy" ] || continue; case "$policy" in */cairn-native-*[!0-9]*|*/cairn-native-) continue ;; esac; if [ -L "$policy" ] || [ ! -f "$policy" ]; then echo "Unrecognized native sudo policy: host-owner review required." >&2; exit 1; fi; if cairn_policy "$policy"; then rm -f -- "$policy" || exit; else policy_status=$?; [ "$policy_status" -eq 1 ] || exit "$policy_status"; fi; done; rm -rf /opt/cairn-native /var/lib/cairn-native && { userdel cairn-workload || ! id cairn-workload >/dev/null 2>&1; } && rm -f /etc/systemd/system/cairn-native-node.service && systemctl daemon-reload && { if [ ! -e /var/lib/cairn-node ]; then systemctl disable --now cairn-node-gateway 2>/dev/null; rm -rf /etc/systemd/system/cairn-node-gateway.service /opt/cairn-gateway /var/lib/cairn-gateway; systemctl daemon-reload; fi; }'`
// The node a guided backend switch replaces, while the new runtime's enrollment is pending.
const replacing = ref<ExecutionNode | null>(null)
// Once the replacement connects and its setup steps end, retiring the previous node is offered.
let replacement: { previous: string, next: string } | null = null
const retiring = ref<{ previous: ExecutionNode, next: ExecutionNode } | null>(null)
const uninstallCommands = {
  firecracker: `sudo systemctl disable --now cairn-node; sudo docker rm -f cairn-execution-node; sudo rm -rf /etc/systemd/system/cairn-node.service /opt/cairn-node /var/lib/cairn-node; sudo sh -c 'if [ ! -e /var/lib/cairn-native ]; then systemctl disable --now cairn-node-gateway 2>/dev/null; rm -rf /etc/systemd/system/cairn-node-gateway.service /opt/cairn-gateway /var/lib/cairn-gateway; systemctl daemon-reload; fi'`,
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
// The machine shown in the side panel, followed through polling by its id.
const openedId = ref<string | null>(null)
const machinesHeading = ref<HTMLElement>()
const loaded = ref(false)
// A failed inventory refresh concerns the page, not the machine whose panel is open; the next success clears it.
const refreshError = ref('')
// Trusted people load separately; their failure concerns native configuration, not the inventory.
const callersError = ref('')
// Action dialogs show their own errors; adding a machine also shows refresh failures, as it waits on refreshes.
const shownError = computed(() => error.value || refreshError.value)
const errorNodeId = ref<string | null>(null)
// Behind dialogs and panels, the page names the machine an action error concerns.
const pageError = computed(() => {
  const machine = errorNodeId.value && nodes.value.find(node => node.id === errorNodeId.value)
  const action = error.value && machine ? `${machine.name}: ${error.value}` : error.value
  return [action, refreshError.value, callersError.value].filter(Boolean).join(' · ')
})
const opened = computed(() => nodes.value.find(node => node.id === openedId.value) ?? null)
const active = computed(() => nodes.value.filter(node => !node.revoked))
const revoked = computed(() => nodes.value.filter(node => node.revoked))
const visible = computed(() => showRevoked.value ? nodes.value : active.value)
// When the built-in runner's contact last changed, as seen by this device: an unchanged contact means it went stale.
const runner = ref<RunnerObservation | null>(null)
const clock = computed(() => fleetClock(nodes.value, runner.value))
// How long each machine has stayed unready without a reported error, across successful listings.
const unready = ref<UnreadyObservations>({})
const summary = computed(() => fleetSummary(nodes.value, Date.now(), clock.value, runner.value, unready.value))
let timer: ReturnType<typeof setInterval> | undefined

// A machine that leaves the listing while its panel is open closes it; focus returns to the list.
watch(opened, async (now, before) => {
  if (!before || now || openedId.value !== before.id)
    return
  openedId.value = null
  await nextTick()
  if (!document.activeElement || document.activeElement === document.body)
    machinesHeading.value?.focus()
})

// A dialog opens on a clean slate: errors from earlier actions are not its own.
watch(() => [adding.value, editing.value?.id, grantingId.value, cleaning.value?.id, revoking.value?.id, retiring.value?.previous.id, recovery.value !== null], (current, previous) => {
  if (current.some((value, index) => value && !previous?.[index])) {
    error.value = ''
    errorNodeId.value = null
  }
})
// The newly enrolled native node whose "Who can use it?" step follows its configuration.
let grantAfterConfigure: string | null = null

// Polls and post-action refreshes can overlap: an answer older than one already applied is dropped.
// Slow answers still apply when nothing newer arrived first, so slow listings keep the page current.
let started = 0
let applied = 0
let failed = 0

async function load() {
  const request = ++started
  try {
    const listed = await api<ExecutionNode[]>('/nodes')
    if (request < applied)
      return
    applied = request
    nodes.value = listed
    runner.value = observeRunner(nodes.value, runner.value)
    unready.value = observeUnready(nodes.value, unready.value, Date.now(), fleetClock(nodes.value, runner.value), runner.value)
    loaded.value = true
    // An older answer does not hide a newer listing's failure.
    if (request > failed)
      refreshError.value = ''
  }
  catch (e) {
    if (request > applied) {
      refreshError.value = (e as Error).message
      failed = request
    }

    return
  }

  if (nodes.value.some(node => node.capabilities.backend === 'native')) {
    try {
      nativeCallers.value = await api('/nodes/native-callers')
      callersError.value = ''
    }
    catch (e) {
      callersError.value = `Trusted people could not be loaded: ${(e as Error).message}`
    }
  }
  else {
    callersError.value = ''
  }

  if (!enrollment.value)
    return
  const invitation = enrollment.value
  const connected = nodes.value.find(node => !node.local && !node.revoked
    && nodeBackend(node) === (invitation.backend ?? 'firecracker')
    && node.id === invitation.nodeId)
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

// `nodeId` names the machine an action concerns, so its panel shows only its own errors.
async function action(operation: () => Promise<void>, nodeId: string | null = null) {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  errorNodeId.value = null
  try {
    await operation()
    await load()
  }
  catch (e) {
    error.value = (e as Error).message
    errorNodeId.value = nodeId
  }
  finally {
    busy.value = false
  }
}

function enroll() {
  return action(async () => {
    enrollment.value = await createNodeEnrollment(api, {
      name: name.value,
      managerUrl: managerUrl.value.trim() || null,
      backend: enrollmentBackend.value,
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

// Reopening a machine starts clean: its earlier error was seen. Other machines' errors stay on the page.
function openNode(node: ExecutionNode) {
  if (errorNodeId.value === node.id) {
    error.value = ''
    errorNodeId.value = null
  }

  openedId.value = node.id
}

// The switch continues in its own steps; the panel would show the node being replaced.
// The panel closes first, so it returns focus to the row before the next dialog opens.
async function switchFromPanel(node: ExecutionNode) {
  openedId.value = null
  await nextTick()
  switchBackend(node)
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
    const result = await revokeNode(api, { nodeId: pair.previous.id, releasePlacements: true })
    retiring.value = null
    const released = result.releasedPlacements ?? 0
    notify(released ? `${pair.previous.name} revoked; ${released} ${released === 1 ? 'conversation returns' : 'conversations return'} to automatic placement` : `${pair.previous.name} revoked`)
  }, pair.previous.id)
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
    const configuration: NodeConfiguration = {
      revision: node.revision,
      name: node.name,
      tags: tags.value.split(',').map(tag => tag.trim()).filter(Boolean),
      limits,
      slots: node.slots ?? null,
      accepting: node.accepting,
      nativeEnabled: node.capabilities.backend === 'native' ? node.nativeEnabled : null,
      nativeAccounts: node.capabilities.backend === 'native' ? node.nativeAccounts : null,
      confirmNativeAdministration: node.capabilities.backend === 'native' && node.nativeModeRequest?.mode === 'administrator',
    }
    await api(`/nodes/${node.id}`, { method: 'PUT', body: JSON.stringify(configuration) })
    editing.value = null
    notify('Node saved')
    if (grantAfterConfigure === node.id) {
      grantAfterConfigure = null
      grant(node)
    }
  }, node.id)
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
    await releaseNodeReservation(api, { reservationId: reservation.id })
    notify('Exclusive release requested; suspended work resumes after confirmation')
  }, node.id)
}

function saveGrants() {
  const node = granting.value
  if (!node)
    return
  // `add` only grants the chosen agents: finishing never revokes one allowed meanwhile.
  const agentIds = granted.value.filter(agentId => !allowed(agentId) && !mismatch(agentId))
  return action(async () => {
    if (agentIds.length) {
      await api(`/nodes/${node.id}/agents`, { method: 'PUT', body: JSON.stringify({ agentIds, add: true, ...(node.nativeModeRequest?.mode === 'administrator' ? { confirmNativeAdministration: true } : {}) }) })
    }

    granting.value = null
    notify(`${node.name} is ready`)
    await refresh()
    offerRetirement(node.id)
  }, node.id)
}

function freeStaleDisks() {
  const node = cleaning.value
  if (!node)
    return
  return action(async () => {
    const result = await api<{ freedMiB: number, failed: number }>(`/nodes/${node.id}/stale-disks/delete`, { method: 'POST', body: '{}' })
    cleaning.value = null
    notify(result.failed ? `Freed ${formatMiB(result.freedMiB)}; ${result.failed} disks are in use or the node is unreachable` : `Freed ${formatMiB(result.freedMiB)}`)
  }, node.id)
}

/** Why an agent cannot run on the node being granted: the server only accepts agents of its backend. */
function mismatch(agentId: string) {
  const agent = state.agents.find(item => item.id === agentId)
  const native = granting.value?.capabilities.backend === 'native'
  if (!agent || agent.access?.backend === 'auto' || (agent.access?.backend === 'native') === native)
    return ''
  return native ? 'Runs in a Firecracker VM: set it to native execution on its page first' : 'Uses native execution: only native nodes can run it'
}

function allNodes(agentId: string) {
  if (granting.value?.nativeModeRequest?.mode === 'administrator' && !granting.value.nativeReviewedAgents?.includes(agentId))
    return false
  return (!mismatch(agentId) && state.agents.some(agent => agent.id === agentId && agent.access?.nodes === null))
    || (granting.value?.agents?.some(agent => agent.id === agentId && agent.allNodes) ?? false)
}

/** Already allowed on the node being granted, from its latest data, or on every node. */
function allowed(agentId: string) {
  if (granting.value?.nativeModeRequest?.mode === 'administrator' && !granting.value.nativeReviewedAgents?.includes(agentId))
    return false
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

function revoke() {
  const node = revoking.value
  if (!node)
    return
  return action(async () => {
    await revokeNode(api, { nodeId: node.id, releasePlacements: false })
    // The revoked row leaves the list: once the panel is gone, the dialog returns focus to the list.
    if (openedId.value === node.id) {
      openedId.value = null
      await nextTick()
    }

    revoking.value = null
  }, node.id)
}
</script>

<template>
  <div class="mx-auto max-w-240 px-8 py-8 phone:px-4">
    <header class="page-heading mb-6 flex items-end justify-between gap-5 phone:mb-5 phone:items-center">
      <div>
        <h1>Nodes</h1>
        <p class="mt-2 text-[13px] text-muted phone:hidden">
          Machines that run your agents’ conversations.
        </p>
      </div>
      <div class="flex shrink-0 gap-2 phone:flex-none!">
        <UiButton aria-label="Synchronization settings" title="Synchronization settings" @click="configureRecovery">
          <Icon :name="Settings2" :size="15" /><span class="phone:hidden">Synchronization</span>
        </UiButton>
        <UiButton variant="primary" aria-label="Add a machine" @click="addMachine">
          <Icon :name="Plus" :size="16" /><span class="phone:hidden">Add a machine</span>
        </UiButton>
      </div>
    </header>
    <section v-if="loaded" aria-label="Fleet summary">
      <dl class="mb-5 grid grid-cols-3 gap-3 phone:gap-2">
        <div class="rounded-xl border border-line bg-surface flex flex-col-reverse px-4 py-3 phone:px-3">
          <dt class="text-xs text-muted">
            connected
          </dt>
          <dd class="font-heading text-xl font-bold tabular-nums">
            {{ summary.connected }}<span class="text-sm font-normal text-muted"> / {{ summary.machines }}</span>
          </dd>
        </div>
        <div class="rounded-xl border border-line bg-surface flex flex-col-reverse px-4 py-3 phone:px-3">
          <dt class="text-xs text-muted">
            slots in use
          </dt>
          <dd class="font-heading text-xl font-bold tabular-nums">
            {{ summary.usedSlots }}<span class="text-sm font-normal text-muted"> / {{ summary.totalSlots }}</span>
          </dd>
        </div>
        <div class="rounded-xl border bg-surface flex flex-col-reverse px-4 py-3 phone:px-3" :class="summary.attention ? 'border-coral/40' : 'border-line'">
          <dt class="text-xs text-muted">
            {{ summary.attention === 1 ? 'needs attention' : 'need attention' }}
          </dt>
          <dd class="font-heading text-xl font-bold tabular-nums" :class="{ 'text-coral': summary.attention }">
            {{ summary.attention }}
          </dd>
        </div>
      </dl>
    </section>
    <UiAlert v-if="pageError && !adding" class="mb-4">
      {{ pageError }}
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
          Cairn introduces the machine to the manager: no manager address, certificate or open port is needed. They connect directly when the networks allow it. Otherwise their traffic, including conversation disks, passes through Cairn’s servers encrypted end to end, so Cairn cannot read it.
        </p>
        <UiAlert v-if="shownError">
          {{ shownError }}
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
          <details class="text-sm" :open="managerUrl !== ''">
            <summary class="cursor-pointer text-muted">
              Connect directly instead
            </summary>
            <label class="mt-2 block">Direct manager address<input
              v-model="managerUrl"
              type="url"
              placeholder="https://manager.vpn.example:4310"
              aria-describedby="node-manager-address-help"
            ></label>
            <p id="node-manager-address-help" class="mt-1 text-muted">
              Optional. A LAN or VPN address keeps node traffic off Cairn’s servers even when the networks prevent a direct connection. Use HTTPS with a certificate trusted by the node; HTTP is allowed only on loopback for local tests. This is the manager’s address, not the beacon app’s address.
            </p>
          </details>
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
            <template v-if="enrollmentCommand">
              <p>Meanwhile, on a machine with the matching <code>cairn</code> binary, run:</p>
              <code class="block break-all select-all" aria-label="Node enrollment command">{{ enrollmentCommand }}</code>
              <p>Paste this single-use code, press Enter, then Ctrl+D to finish standard input. It expires {{ new Date(enrollment.expiresAt).toLocaleString() }}:</p>
            </template>
          </template>
          <code class="block break-all select-all" aria-label="Single-use enrollment code">{{ enrollment.code }}</code>
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
    <h2
      id="machines-heading"
      ref="machinesHeading"
      class="eyebrow mb-3 rounded focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
      tabindex="-1"
    >
      Machines
    </h2>
    <section class="overflow-hidden rounded-2xl border border-line bg-surface shadow-arcade" aria-labelledby="machines-heading">
      <div class="grid grid-cols-[minmax(0,1fr)_7rem_5.5rem_8.5rem_5rem_1rem] gap-4 bg-inset px-4.5 py-2 text-[10.5px] tracking-[.06em] text-muted uppercase max-[1024px]:grid-cols-[minmax(0,1fr)_7rem_5.5rem_1rem] phone:hidden" aria-hidden="true">
        <span>Machine</span><span>Runtime</span><span>Slots</span><span class="max-[1024px]:hidden">Usage</span><span class="max-[1024px]:hidden">Agents</span><span />
      </div>
      <ul aria-label="Machines" class="[&>li:first-child>button]:border-t-0">
        <li v-for="node in visible" :key="node.id">
          <NodeRow
            :node="node"
            :clock="clock"
            :runner="runner"
            :unready="unready"
            :selected="node.id === openedId"
            @open="openNode(node)"
          />
        </li>
      </ul>
      <p v-if="!loaded && !shownError" class="px-4.5 py-6 text-sm text-muted" role="status">
        Loading machines…
      </p>
      <p v-else-if="loaded && !visible.length" class="px-4.5 py-6 text-sm text-muted">
        {{ revoked.length ? 'No active machine. Revoked machines are hidden.' : 'No machine yet. Add one to run your agents’ conversations on it.' }}
      </p>
    </section>
    <UiButton
      v-if="revoked.length"
      class="mt-4"
      size="small"
      variant="default"
      @click="showRevoked = !showRevoked"
    >
      {{ showRevoked ? 'Hide revoked machines' : `Show revoked machines (${revoked.length})` }}
    </UiButton>
    <NodeDetail
      v-if="opened"
      :key="opened.id"
      :node="opened"
      :clock="clock"
      :runner="runner"
      :unready="unready"
      :return-focus="visible.some(node => node.id === opened!.id) ? undefined : machinesHeading"
      :busy="busy"
      :error="errorNodeId === opened.id ? error : ''"
      :refresh-error="refreshError"
      @close="openedId = null"
      @changed="load"
      @configure="edit(opened)"
      @grant="grant(opened)"
      @clean="cleaning = opened"
      @release="releaseExclusive(opened)"
      @switch="switchFromPanel(opened)"
      @revoke="revoking = opened"
    />
    <Modal
      v-if="recovery"
      sheet
      title="S3 synchronization"
      @close="recovery = null"
    >
      <form class="node-form" @submit.prevent="saveRecovery">
        <p class="text-sm text-muted">
          How often disk changes are published to S3 and how long nodes wait before pausing. The defaults suit most setups.
        </p>
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
        <p v-if="granting.nativeModeRequest?.mode === 'administrator'" class="text-power">
          Granting access here gives these agents unrestricted passwordless sudo on the real host. Every trusted person may direct them to administer this machine. Root can read local credentials and defeat cooperative limits. Saving confirms this access.
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
          <p v-if="editing.nativeModeRequest?.mode === 'administrator'" class="text-power">
            Every trusted person selected here may direct unrestricted host administration through admitted agents. Saving confirms this access.
          </p>
          <label class="access-row"><input v-model="editing.nativeEnabled" type="checkbox">Enable native execution</label>
          <template v-if="nativeCallers.length">
            <p id="native-trusted-people" class="mt-4 mb-2!">
              Trusted people
            </p>
            <div class="grid gap-2" role="group" aria-labelledby="native-trusted-people">
              <label v-for="person in nativeCallers" :key="person.id" class="access-row"><input v-model="editing.nativeAccounts" type="checkbox" :value="person.id"><span class="min-w-0 break-all">{{ person.email }}</span></label>
            </div>
          </template>
          <UiAlert v-if="callersError">
            {{ callersError }}
          </UiAlert>
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
          <p v-if="!editing.exclusiveReservations">
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
      :return-focus="opened ? undefined : machinesHeading"
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
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <div>
          <UiButton variant="danger" :disabled="busy" @click="revoke">
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
@media (max-width: 640px) { .node-form { padding: 20px; } .form-grid { grid-template-columns: 1fr; } }
</style>
