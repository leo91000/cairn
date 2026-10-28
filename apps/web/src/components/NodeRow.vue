<script setup lang="ts">
import type { ExecutionNode } from '../../../../packages/contracts/nodes'
import { computed } from 'vue'
import {
  formatMiB,
  nodeAdministrator,
  nodeHealth,
  usedPercent,
} from '../../../../packages/contracts/nodes'
import { ChevronRight } from '../icons'
import AgentAvatar from './AgentAvatar.vue'
import Icon from './Icon.vue'
import NodeHealthBadge from './NodeHealthBadge.vue'
import NodeMeter from './NodeMeter.vue'

// One machine in the fleet: whether it takes work, its free slots, memory and disk in use, and who may run there.
const props = defineProps<{ node: ExecutionNode, selected?: boolean }>()
defineEmits<{ open: [] }>()
const health = computed(() => nodeHealth(props.node))
const live = computed(() => !['offline', 'revoked'].includes(health.value.state))
const slots = computed(() => props.node.slots ?? 0)
const free = computed(() => props.node.availableSlots ?? slots.value)
const runtime = computed(() => props.node.capabilities.backend === 'native' ? nodeAdministrator(props.node) ? 'Native · admin' : 'Native' : 'VM')
const agents = computed(() => props.node.agents ?? [])
</script>

<template>
  <button
    type="button"
    class="node-row group grid w-full grid-cols-[minmax(0,1fr)_7rem_5.5rem_8.5rem_4.5rem_1rem] items-center gap-4 border-t border-line px-4.5 py-3.5 text-left transition-colors hover:bg-hover/40 focus-visible:-outline-offset-2 focus-visible:outline-2 focus-visible:outline-accent tablet:grid-cols-[minmax(0,1fr)_7rem_5.5rem_1rem] phone:grid-cols-[minmax(0,1fr)_auto] phone:gap-3 phone:px-4"
    :class="{ 'bg-soft/40': selected }"
    :aria-label="`${node.name}, ${health.label}`"
    @click="$emit('open')"
  >
    <span class="min-w-0">
      <span class="flex min-w-0 items-center gap-2">
        <span class="truncate text-[14px] font-semibold">{{ node.name }}</span>
        <NodeHealthBadge :health="health" />
      </span>
      <span class="block truncate text-xs" :class="['offline', 'attention'].includes(health.state) ? 'text-coral' : 'text-muted'">{{ node.local ? `Built-in runner · ${health.detail}` : health.detail }}</span>
    </span>
    <span class="phone:hidden">
      <span class="inline-flex h-[22px] items-center rounded-full px-2.5 text-[11.5px] font-semibold whitespace-nowrap" :class="runtime === 'Native · admin' ? 'bg-power-surface text-power' : 'bg-hover text-muted'">{{ runtime }}</span>
    </span>
    <span class="text-sm tabular-nums" :class="{ 'text-muted': !live }">
      <template v-if="live && slots">
        <b class="font-semibold">{{ free }}</b><span class="text-xs text-muted"> / {{ slots }} free</span>
      </template>
      <template v-else>—</template>
    </span>
    <span class="grid gap-1.5 text-[11px] text-muted tablet:hidden" :class="{ 'opacity-40': !live }">
      <template v-if="node.usage && live">
        <span class="grid grid-cols-[2.2rem_minmax(0,1fr)] items-center gap-2" :title="`${formatMiB(node.usage.memoryMiB)} of ${formatMiB(node.limits.memoryMiB)} RAM`">RAM<NodeMeter :used="usedPercent(node.usage.memoryMiB, node.limits.memoryMiB)" :label="`${node.name} RAM in use`" /></span>
        <span class="grid grid-cols-[2.2rem_minmax(0,1fr)] items-center gap-2" :title="`${formatMiB(node.usage.diskMiB)} of ${formatMiB(node.limits.diskMiB)} disk`">Disk<NodeMeter :used="usedPercent(node.usage.diskMiB, node.limits.diskMiB)" :label="`${node.name} disk in use`" /></span>
      </template>
      <template v-else>
        Usage unavailable
      </template>
    </span>
    <span class="flex items-center tablet:hidden" :aria-label="`${agents.length} ${agents.length === 1 ? 'agent' : 'agents'}`">
      <AgentAvatar
        v-for="agent in agents.slice(0, 3)"
        :key="agent.id"
        :name="agent.name"
        :identity="agent.id"
        :size="22"
        class="-ml-1.5 rounded-[7px] ring-2 ring-surface first:ml-0"
      />
      <span v-if="agents.length > 3" class="ml-1 text-xs text-muted">+{{ agents.length - 3 }}</span>
      <span v-if="!agents.length" class="text-xs text-muted">None</span>
    </span>
    <Icon :name="ChevronRight" :size="16" class="text-subtle transition-transform group-hover:translate-x-0.5 phone:hidden" />
  </button>
</template>
