<script setup lang="ts">
import type { ExecutionNode } from '../../../../packages/contracts/nodes'
import type { AgentAccess, ChipTone } from '../agent-access'
import { computed, ref } from 'vue'
import { formatMiB } from '../../../../packages/contracts/nodes'
import {
  backendLabels,
  effectiveExclusive,
  nodeBackend,
  nodeName,
  nodesFor,
  nodeState,
  runProblem,
  runsOn,
  supportsExclusive,
  unknownNode,
  withRun,
} from '../agent-access'
import { Info, Search, X } from '../icons'
import AccessChip from './AccessChip.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import UiSwitch from './UiSwitch.vue'

// Per node: may the agent run there, and may it reserve the node exclusively.
// `readOnlyGrants` says why exclusive reservations cannot be changed here (an older installation).
const props = defineProps<{ nodes: ExecutionNode[], readOnlyGrants?: string }>()
const access = defineModel<AgentAccess>('access', { required: true })
// Nodes the agent may reserve exclusively, a live grant kept beside `access`.
const exclusiveNodes = defineModel<string[]>('exclusiveNodes', { required: true })
const query = ref('')
const selectedOnly = ref(false)
const unavailableOnly = ref(false)
// VM and native nodes alike: each conversation keeps the backend it started on.
const available = computed(() => nodesFor(props.nodes))
const searchable = computed(() => available.value.length >= 8)
const visible = computed(() => {
  const words = query.value.trim().toLowerCase()
  return available.value.filter(node => (!words || [nodeName(node, node.id), ...node.tags, ...(node.systemTags ?? [])].join(' ').toLowerCase().includes(words))
    && (!selectedOnly.value || runsOn(access.value, node.id) || exclusiveNodes.value.includes(node.id))
    && (!unavailableOnly.value || (!unknownNode(node) && nodeState(node).state !== 'online')))
})
// Run permissions on nodes revoked or deleted: kept until removed.
const strayRuns = computed(() => (access.value.nodes ?? []).filter(id => runProblem(id, props.nodes)))
// Exclusive grants whose node was revoked or deleted.
const missing = computed(() => exclusiveNodes.value.filter(id => !available.value.some(node => node.id === id)))
const exclusiveCount = computed(() => effectiveExclusive({ access: access.value, exclusiveNodes: exclusiveNodes.value }, props.nodes).length)
const stateTones: Record<string, ChipTone> = {
  online: 'granted',
  offline: 'unavailable',
  paused: 'unavailable',
  update: 'unavailable',
}

const allNodes = computed({
  get: () => access.value.nodes === null,
  // "Choose" starts from the nodes it can use today, so turning "All" off only stops future nodes.
  set: (value: boolean) => {
    access.value.nodes = value ? null : available.value.map(node => node.id)
  },
})

function details(node: ExecutionNode) {
  if (unknownNode(node))
    return 'local'
  return [
    node.local ? 'local' : '',
    [...node.tags, ...(node.systemTags ?? [])].join(', '),
    `${node.capabilities.cpu} CPU`,
    formatMiB(node.capabilities.memoryMiB),
  ].filter(Boolean).join(' · ')
}

function setRun(node: string, on: boolean) {
  // Without permission to run, an exclusive reservation can never be used, unless it is read-only.
  const next = withRun(access.value, exclusiveNodes.value, node, on, Boolean(props.readOnlyGrants))
  access.value.nodes = next.nodes
  exclusiveNodes.value = next.exclusiveNodes
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
    <div class="flex items-center justify-between gap-4 rounded-xl border border-line bg-surface px-5 py-4 phone:px-4">
      <div class="min-w-0">
        <p class="text-sm font-semibold">
          All nodes, including future
        </p>
        <p class="text-xs text-muted">
          New VM machines become usable as soon as they are enrolled; native machines must also enable execution and trust everyone directing the conversation.
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
          <p class="truncate text-2xs" :class="nodeBackend(node) === 'native' ? 'text-power' : 'text-subtle'">
            {{ backendLabels[nodeBackend(node)] }}
          </p>
        </div>
        <div>
          <AccessChip v-if="!unknownNode(node)" :tone="stateTones[nodeState(node).state]!">
            {{ nodeState(node).label }}
          </AccessChip>
        </div>
        <div class="flex items-center gap-3 compact:col-span-2 compact:justify-between compact:border-t compact:border-line/60 compact:pt-2">
          <span class="hidden text-sm compact:inline">Can run its conversations</span>
          <span v-if="access.nodes === null" class="text-xs text-muted">Included · all nodes</span>
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
                v-if="!readOnlyGrants"
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
              :disabled="Boolean(readOnlyGrants) || (Boolean(exclusiveBlocked(node)) && !exclusiveNodes.includes(node.id))"
              :label="`Can reserve ${nodeName(node, node.id)} exclusively`"
              @update:model-value="setExclusive(node.id, $event)"
            />
            <span v-if="exclusiveBlocked(node)" class="text-2xs text-muted compact:hidden">{{ exclusiveBlocked(node) }}</span>
          </span>
        </div>
      </div>
      <div
        v-for="id in strayRuns"
        :key="`run-${id}`"
        role="listitem"
        class="flex flex-wrap items-center gap-3 border-b border-line px-1 py-3.5"
        aria-label="Removed node"
      >
        <span class="min-w-0 flex-1 text-sm text-muted">Removed node · can run its conversations</span>
        <AccessChip tone="missing">
          Not found
        </AccessChip>
        <UiButton size="small" aria-label="Remove permission to run on a removed node" @click="setRun(id, false)">
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
          v-if="!readOnlyGrants"
          size="small"
          aria-label="Remove exclusive reservation on a removed node"
          @click="setExclusive(id, false)"
        >
          <Icon :name="X" :size="13" />Remove
        </UiButton>
      </div>
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
    <p v-if="readOnlyGrants" class="rounded-xl bg-soft px-5 py-3 text-sm" role="note">
      Exclusive reservations: {{ readOnlyGrants }}
    </p>
    <p v-if="exclusiveCount" class="rounded-xl bg-power-surface px-5 py-3 text-xs text-power">
      Exclusive reservation granted on {{ exclusiveCount }} {{ exclusiveCount === 1 ? 'node' : 'nodes' }}. While it holds a reservation, other agents’ conversations on that machine are suspended.
    </p>
  </div>
</template>
