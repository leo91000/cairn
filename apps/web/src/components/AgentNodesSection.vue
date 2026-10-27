<script setup lang="ts">
import type { ExecutionNode } from '../../../../packages/contracts/nodes'
import type { AgentAccess, Backend, ChipTone } from '../agent-access'
import { computed, ref, useId } from 'vue'
import { formatMiB } from '../../../../packages/contracts/nodes'
import {
  backendOf,
  backendOptions,
  effectiveExclusive,
  nativeHelp,
  nodeBackend,
  nodeName,
  nodesFor,
  nodeState,
  runProblem,
  runsOn,
  supportsExclusive,
  unknownNode,
  withAllNodes,
  withBackend,
  withRun,
} from '../agent-access'
import {
  GripVertical,
  Info,
  Search,
  X,
} from '../icons'
import AccessChip from './AccessChip.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import UiSwitch from './UiSwitch.vue'
import VirtualSelect from './VirtualSelect.vue'

// Per node: may the agent run there, and may it reserve the node exclusively.
const props = defineProps<{ nodes: ExecutionNode[], agentId?: string }>()
const access = defineModel<AgentAccess>('access', { required: true })
// Nodes the agent may reserve exclusively, a live grant kept beside `access`.
const exclusiveNodes = defineModel<string[]>('exclusiveNodes', { required: true })
const nodePriorities = defineModel<string[]>('nodePriorities', { default: () => [] })
const query = ref('')
const selectedOnly = ref(false)
const unavailableOnly = ref(false)
const backend = computed(() => backendOf(access.value))
// Automatic placement offers both execution kinds within the agent's grants.
const available = computed(() => nodesFor(props.nodes, backend.value))
const searchable = computed(() => available.value.length >= 8)
const visible = computed(() => {
  const words = query.value.trim().toLowerCase()
  return available.value.filter(node => (!words || [nodeName(node, node.id), ...node.tags, ...(node.systemTags ?? [])].join(' ').toLowerCase().includes(words))
    && (!selectedOnly.value || runsOn(access.value, node.id) || exclusiveNodes.value.includes(node.id))
    && (!unavailableOnly.value || (!unknownNode(node) && nodeState(node).state !== 'online')))
})
// Run permissions on nodes revoked, deleted or of the other backend: kept until removed.
const strayRuns = computed(() => (access.value.nodes ?? [])
  .map(id => ({ id, problem: runProblem(id, access.value, props.nodes), node: props.nodes.find(node => node.id === id) }))
  .filter(item => item.problem))
// Exclusive grants whose node was revoked or deleted.
const missing = computed(() => exclusiveNodes.value.filter(id => !available.value.some(node => node.id === id)))
const exclusiveCount = computed(() => effectiveExclusive({ access: access.value, exclusiveNodes: exclusiveNodes.value }, props.nodes).length)
const stateTones: Record<string, ChipTone> = {
  online: 'granted',
  offline: 'unavailable',
  paused: 'unavailable',
  update: 'unavailable',
}

// A different backend runs elsewhere: the node choices and exclusive reservations start over.
const backendChoice = computed({
  get: () => backend.value,
  set: (value: string) => {
    if (value === backend.value)
      return
    const next = value === 'auto' ? { nodes: access.value.nodes, exclusiveNodes: exclusiveNodes.value } : withBackend()
    access.value.backend = value as Backend
    access.value.nodes = next.nodes
    exclusiveNodes.value = next.exclusiveNodes
    if (value !== 'auto')
      nodePriorities.value = []
  },
})

const allNodes = computed({
  get: () => access.value.nodes === null,
  // "Choose" starts from the nodes it can use today, so turning "All" off only stops future nodes.
  set: (value: boolean) => {
    const next = withAllNodes(access.value, nodePriorities.value, props.nodes, value, props.agentId)
    access.value.nodes = next.nodes
    nodePriorities.value = next.nodePriorities
  },
})

function details(node: ExecutionNode) {
  if (unknownNode(node))
    return 'local'
  return [
    node.local ? 'local' : '',
    node.nativeModeRequest?.mode === 'administrator' ? 'Native administrator · unrestricted host sudo; owner review required' : '',
    [...node.tags, ...(node.systemTags ?? [])].join(', '),
    `${node.capabilities.cpu} CPU`,
    formatMiB(node.capabilities.memoryMiB),
  ].filter(Boolean).join(' · ')
}

function setRun(node: string, on: boolean) {
  // Without permission to run, an exclusive reservation can never be used, unless it is read-only.
  const next = withRun(access.value, exclusiveNodes.value, node, on)
  access.value.nodes = next.nodes
  exclusiveNodes.value = next.exclusiveNodes
  if (!on)
    nodePriorities.value = nodePriorities.value.filter(id => id !== node)
}

const priorityNodes = computed(() => nodePriorities.value.map(id => ({ id, node: props.nodes.find(node => node.id === id) })))
const priorityList = ref<HTMLElement>()
const dragHelp = useId()
const dragging = ref<{
  id: string
  pointer: number
  startY: number
  active: boolean
  target?: string
}>()
const announcement = ref('')

function startPriorityDrag(event: PointerEvent, id: string) {
  if (!event.isPrimary || event.button !== 0 || nodePriorities.value.length < 2)
    return
  const handle = event.currentTarget as HTMLElement
  handle.setPointerCapture(event.pointerId)
  dragging.value = {
    id,
    pointer: event.pointerId,
    startY: event.clientY,
    active: false,
  }
}

function trackPriorityDrag(event: PointerEvent) {
  const drag = dragging.value
  if (!drag || drag.pointer !== event.pointerId)
    return
  if (Math.abs(event.clientY - drag.startY) >= 5)
    drag.active = true
  if (!drag.active)
    return

  const row = document.elementFromPoint(event.clientX, event.clientY)?.closest<HTMLElement>('[data-priority-id]')
  drag.target = row && priorityList.value?.contains(row) ? row.dataset.priorityId : undefined
}

function finishPriorityDrag(event: PointerEvent) {
  const drag = dragging.value
  if (!drag || drag.pointer !== event.pointerId)
    return
  trackPriorityDrag(event)
  dragging.value = undefined
  if (!drag.active || !drag.target)
    return

  const order = [...nodePriorities.value]
  const from = order.indexOf(drag.id)
  const to = order.indexOf(drag.target)
  if (from < 0 || to < 0 || from === to)
    return

  order.splice(from, 1)
  order.splice(to, 0, drag.id)
  nodePriorities.value = order
  announcement.value = `${nodeName(props.nodes.find(node => node.id === drag.id), drag.id)} moved to priority ${to + 1}`
}

function movePriority(index: number, direction: number) {
  const order = [...nodePriorities.value]
  const other = index + direction
  if (other < 0 || other >= order.length) {
    return
  }

  [order[index], order[other]] = [order[other]!, order[index]!]
  nodePriorities.value = order
}

function prioritize(node: string) {
  if (!nodePriorities.value.includes(node))
    nodePriorities.value = [...nodePriorities.value, node]
}

function setExclusive(node: string, on: boolean) {
  const granted = exclusiveNodes.value.filter(id => id !== node)
  exclusiveNodes.value = on ? [...granted, node] : granted
}

function exclusiveBlocked(node: ExecutionNode) {
  if (!runsOn(access.value, node.id))
    return 'Requires permission to run'
  if (nodeBackend(node) === 'native')
    return 'Unavailable for native execution'
  if (!supportsExclusive(node))
    return 'Unavailable on this runtime'
  return ''
}
</script>

<template>
  <div class="grid min-w-0 gap-5">
    <div class="grid gap-2">
      <VirtualSelect v-model="backendChoice" label="Agent execution backend" :options="[...backendOptions]" />
      <p v-if="backend !== 'firecracker'" class="rounded-xl bg-power-surface px-5 py-3 text-xs text-power" role="note">
        {{ nativeHelp }}
      </p>
    </div>

    <div class="grid gap-2">
      <p class="text-sm font-semibold">
        This agent’s node priorities
      </p>
      <p class="text-xs text-muted">
        First available authorized node wins. Other authorized nodes remain a fallback. Existing conversations keep their environment.
      </p>
      <p :id="dragHelp" class="text-xs text-muted">
        Drag a handle onto another node to change its priority, or use Up and Down.
      </p>
      <div
        ref="priorityList"
        class="grid gap-2"
        role="list"
        aria-label="Node priorities"
      >
        <div
          v-for="(item, index) in priorityNodes"
          :key="item.id"
          :data-priority-id="item.id"
          role="listitem"
          class="flex flex-wrap items-center gap-2 rounded-xl border p-2"
          :class="[
            dragging?.active && dragging.target === item.id && dragging.id !== item.id ? 'border-accent bg-soft' : 'border-line',
            dragging?.active && dragging.id === item.id ? 'opacity-50' : '',
          ]"
        >
          <button
            type="button"
            class="grid size-11 shrink-0 touch-none select-none place-items-center rounded-lg text-muted hover:bg-hover focus-visible:outline-2 focus-visible:outline-accent enabled:cursor-grab disabled:opacity-40"
            :disabled="priorityNodes.length < 2"
            :aria-label="`Drag ${nodeName(item.node, item.id)} to reorder`"
            :aria-describedby="dragHelp"
            @pointerdown="startPriorityDrag($event, item.id)"
            @pointermove="trackPriorityDrag"
            @pointerup="finishPriorityDrag"
            @pointercancel="dragging = undefined"
            @lostpointercapture="dragging = undefined"
            @keydown.esc="dragging = undefined"
          >
            <Icon :name="GripVertical" :size="18" />
          </button>
          <span class="min-w-0 flex-1 break-words text-sm">{{ index + 1 }}. {{ nodeName(item.node, item.id) }}</span>
          <div class="flex flex-wrap gap-2 phone:w-full phone:justify-end">
            <UiButton :disabled="index === 0" :aria-label="`Move ${nodeName(item.node, item.id)} up`" @click="movePriority(index, -1)">
              Up
            </UiButton>
            <UiButton :disabled="index === priorityNodes.length - 1" :aria-label="`Move ${nodeName(item.node, item.id)} down`" @click="movePriority(index, 1)">
              Down
            </UiButton>
            <UiButton :aria-label="`Remove priority for ${nodeName(item.node, item.id)}`" @click="nodePriorities = nodePriorities.filter(id => id !== item.id)">
              Remove
            </UiButton>
          </div>
        </div>
      </div>
      <p class="sr-only" role="status">
        {{ announcement }}
      </p>
      <div class="flex flex-wrap gap-2">
        <UiButton
          v-for="node in available.filter(node => !nodePriorities.includes(node.id))"
          :key="node.id"
          :disabled="!runsOn(access, node.id)"
          @click="prioritize(node.id)"
        >
          Prioritize {{ nodeName(node, node.id) }}
        </UiButton>
      </div>
    </div>

    <div class="flex items-center justify-between gap-4 rounded-xl border border-line bg-surface px-5 py-4 phone:px-4">
      <div class="min-w-0">
        <p class="text-sm font-semibold">
          All nodes, including future
        </p>
        <p class="text-xs text-muted">
          {{ backend !== 'firecracker' ? 'Native machines must also enable execution and trust everyone directing the conversation. Administrator nodes require explicit owner review on that node; all-node grants never silently grant host root.' : 'New VM machines become usable by this agent as soon as they are enrolled.' }}
        </p>
      </div>
      <UiSwitch v-model="allNodes" label="All nodes, including future" />
    </div>

    <div v-if="searchable" class="flex min-w-0 flex-wrap items-center gap-2">
      <label class="search-field flex min-w-0 flex-1 basis-56 flex-row items-center gap-[7px] rounded-[7px] border border-line bg-raised px-2.5 py-0 text-subtle"><Icon :name="Search" :size="16" /><input
        v-model="query"
        type="search"
        placeholder="Search a node or a tag"
        aria-label="Search nodes"
      ></label>
      <button
        type="button"
        class="min-h-8 rounded-full border px-3 text-2xs font-semibold phone:min-h-11"
        :class="selectedOnly ? 'border-accent bg-soft text-accent' : 'border-line text-muted hover:bg-hover'"
        :aria-pressed="selectedOnly"
        @click="selectedOnly = !selectedOnly"
      >
        Selected
      </button>
      <button
        type="button"
        class="min-h-8 rounded-full border px-3 text-2xs font-semibold phone:min-h-11"
        :class="unavailableOnly ? 'border-accent bg-soft text-accent' : 'border-line text-muted hover:bg-hover'"
        :aria-pressed="unavailableOnly"
        @click="unavailableOnly = !unavailableOnly"
      >
        Unavailable
      </button>
    </div>

    <div class="min-w-0" role="list" aria-label="Nodes">
      <div class="grid grid-cols-[minmax(0,1fr)_150px_180px_200px] gap-4 border-b border-line px-1 pb-2.5 text-3xs font-semibold uppercase tracking-[.12em] text-subtle compact:hidden" aria-hidden="true">
        <span>Node</span><span>State</span><span>Can run its conversations</span><span>Can reserve exclusively</span>
      </div>
      <div
        v-for="node in visible"
        :key="node.id"
        role="listitem"
        class="grid grid-cols-[minmax(0,1fr)_150px_180px_200px] items-center gap-4 border-b border-line px-1 py-3.5 compact:grid-cols-[minmax(0,1fr)_auto] compact:gap-x-3 compact:gap-y-1"
        :aria-label="nodeName(node, node.id)"
      >
        <div class="min-w-0">
          <p class="truncate text-sm font-semibold">
            {{ nodeName(node, node.id) }}
          </p>
          <p class="truncate text-2xs text-muted">
            {{ details(node) }}
          </p>
        </div>
        <div>
          <AccessChip v-if="!unknownNode(node)" :tone="stateTones[nodeState(node).state]!">
            {{ nodeState(node).label }}
          </AccessChip>
        </div>
        <div class="flex items-center gap-3 compact:col-span-2 compact:justify-between compact:border-t compact:border-line/60 compact:pt-2">
          <span class="hidden text-sm compact:inline">Can run its conversations</span>
          <span v-if="access.nodes === null" class="text-xs text-muted">{{ node.nativeModeRequest?.mode === 'administrator' ? 'Requires owner review on this node' : 'Included · all nodes' }}</span>
          <UiSwitch
            v-else
            :model-value="runsOn(access, node.id)"
            :label="`Can run its conversations on ${nodeName(node, node.id)}`"
            @update:model-value="setRun(node.id, $event)"
          />
        </div>
        <div class="flex min-w-0 items-center gap-3 compact:col-span-2 compact:justify-between compact:border-t compact:border-line/60 compact:pt-2">
          <span class="hidden min-w-0 compact:block">
            <span class="block text-sm" :class="exclusiveBlocked(node) && !exclusiveNodes.includes(node.id) ? 'text-muted' : ''">Can reserve exclusively</span>
            <span class="block text-2xs text-muted">{{ exclusiveBlocked(node) || 'Suspends other conversations' }}</span>
          </span>
          <template v-if="exclusiveNodes.includes(node.id) && exclusiveBlocked(node)">
            <span class="flex min-w-0 flex-wrap items-center gap-2 compact:justify-end">
              <AccessChip tone="unavailable">{{ runsOn(access, node.id) ? 'Inactive: unavailable on this runtime' : 'Inactive: cannot run here' }}</AccessChip>
              <UiButton
                size="small"
                :aria-label="`Remove exclusive reservation on ${nodeName(node, node.id)}`"
                @click="setExclusive(node.id, false)"
              >Remove</UiButton>
            </span>
          </template>
          <span v-else class="flex min-w-0 flex-col items-start gap-0.5 compact:items-end">
            <UiSwitch
              power
              :model-value="exclusiveNodes.includes(node.id)"
              :disabled="(Boolean(exclusiveBlocked(node)) && !exclusiveNodes.includes(node.id))"
              :label="`Can reserve ${nodeName(node, node.id)} exclusively`"
              @update:model-value="setExclusive(node.id, $event)"
            />
            <span v-if="exclusiveBlocked(node)" class="text-2xs text-muted compact:hidden">{{ exclusiveBlocked(node) }}</span>
          </span>
        </div>
      </div>
      <div
        v-for="item in strayRuns"
        :key="`run-${item.id}`"
        role="listitem"
        class="flex flex-wrap items-center gap-3 border-b border-line px-1 py-3.5"
        :aria-label="item.problem === 'missing' ? 'Removed node' : nodeName(item.node, item.id)"
      >
        <span class="min-w-0 flex-1 text-sm text-muted">{{ item.problem === 'missing' ? 'Removed node' : `${nodeName(item.node, item.id)} · other execution backend` }} · can run its conversations</span>
        <AccessChip :tone="item.problem === 'missing' ? 'missing' : 'unavailable'">
          {{ item.problem === 'missing' ? 'Not found' : 'Unavailable' }}
        </AccessChip>
        <UiButton size="small" :aria-label="`Remove permission to run on ${item.problem === 'missing' ? 'a removed node' : nodeName(item.node, item.id)}`" @click="setRun(item.id, false)">
          <Icon :name="X" :size="13" />Remove
        </UiButton>
      </div>
      <div
        v-for="id in missing"
        :key="id"
        role="listitem"
        class="flex flex-wrap items-center gap-3 border-b border-line px-1 py-3.5"
      >
        <span class="min-w-0 flex-1 text-sm text-muted">Removed node · exclusive reservation</span>
        <AccessChip tone="missing">
          Not found
        </AccessChip>
        <UiButton
          size="small"
          aria-label="Remove exclusive reservation on a removed node"
          @click="setExclusive(id, false)"
        >
          <Icon :name="X" :size="13" />Remove
        </UiButton>
      </div>
      <p v-if="backend === 'native' && !available.length" class="py-4 text-sm text-muted">
        No native node enrolled yet. <RouterLink to="/nodes" class="text-accent hover:underline">
          Add one in Nodes ›
        </RouterLink>
      </p>
      <p v-if="!visible.length && available.length" class="py-4 text-sm text-muted">
        No node matches these filters.
      </p>
    </div>

    <div class="rounded-xl bg-soft px-5 py-4 text-xs leading-relaxed text-ink phone:hidden">
      <p><b>Run:</b> its conversations may start on or move to this machine and share its slots and budgets with other agents.</p>
      <p><b>Reserve exclusively:</b> during intensive work the agent may suspend all other conversations on the machine for up to 15 minutes per request (1 hour with renewals). They resume automatically. Requires permission to run.</p>
    </div>
    <details class="hidden rounded-xl bg-soft px-4 py-3 text-xs leading-relaxed phone:block">
      <summary class="flex min-h-11 cursor-pointer items-center gap-2 font-semibold">
        <Icon :name="Info" :size="15" />Run or suspend others: what’s the difference?
      </summary>
      <p class="mt-2">
        <b>Run:</b> its conversations may start on or move to this machine and share its slots and budgets with other agents.
      </p>
      <p class="mt-2">
        <b>Reserve exclusively:</b> during intensive work the agent may suspend all other conversations on the machine for up to 15 minutes per request (1 hour with renewals). They resume automatically. Requires permission to run.
      </p>
    </details>
    <p v-if="exclusiveCount" class="rounded-xl bg-power-surface px-5 py-3 text-xs text-power">
      Exclusive reservation granted on {{ exclusiveCount }} {{ exclusiveCount === 1 ? 'node' : 'nodes' }}. While it holds a reservation, other agents’ conversations on that machine are suspended.
    </p>
  </div>
</template>
