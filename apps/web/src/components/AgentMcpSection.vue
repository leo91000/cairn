<script setup lang="ts">
import type { McpView } from '../../../../packages/contracts/mcp'
import type { AgentAccess, SelectionItem } from '../agent-access'
import { computed } from 'vue'
import AccessChip from './AccessChip.vue'
import AgentMcpTools from './AgentMcpTools.vue'
import AgentSelection from './AgentSelection.vue'

// MCP connections, and per connection either all its enabled tools or a chosen few.
// Tool choices also apply under "All, including future", as on Android.
const props = defineProps<{ mcps: McpView[] }>()
const access = defineModel<AgentAccess>('access', { required: true })
const items = computed<SelectionItem[]>(() => props.mcps.map(connection => ({
  id: connection.id,
  name: connection.name,
  detail: toolSummary(connection),
  unavailable: connection.enabled ? undefined : 'Disabled on the installation',
})))
const selection = computed({
  get: () => access.value.mcps,
  set: (value: string[] | null) => {
    // Leaving "All" starts from every connection, keeping the tools chosen for each.
    if (access.value.mcps === null && value !== null && !value.length)
      value = props.mcps.map(connection => connection.id)
    access.value.mcps = value
    // Tool choices only apply to connections that stay granted.
    if (value !== null)
      access.value.mcpTools = Object.fromEntries(Object.entries(access.value.mcpTools).filter(([id]) => value.includes(id)))
  },
})

function toolSummary(connection: McpView) {
  const enabled = connection.tools.filter(tool => connection.enabledTools === null || connection.enabledTools.includes(tool.name)).length
  const chosen = access.value.mcpTools[connection.id]
  return `${enabled} enabled on the connection · ${chosen ? `${chosen.length} chosen` : 'all enabled tools'}`
}
</script>

<template>
  <AgentSelection
    v-model="selection"
    title="MCP connections"
    noun="connection"
    :items="items"
  >
    <template #all>
      <p>Every MCP connection, including future ones, with all of its enabled tools unless you choose some below.</p>
      <ul v-if="mcps.length" class="mt-3 min-w-0 divide-y divide-line overflow-hidden rounded-xl border border-line bg-surface text-ink" aria-label="MCP connections">
        <li
          v-for="connection in mcps"
          :key="connection.id"
          class="min-w-0"
          :aria-label="connection.name"
        >
          <p class="flex min-h-11 items-center gap-3 px-4 py-2.5 text-sm">
            <span class="min-w-0 flex-1 truncate">{{ connection.name }}</span>
            <AccessChip v-if="!connection.enabled" tone="unavailable">
              Disabled on the installation
            </AccessChip>
          </p>
          <AgentMcpTools v-model:access="access" :connection="connection" />
        </li>
      </ul>
    </template>
    <template #item="{ item, selected }">
      <AgentMcpTools v-if="selected" v-model:access="access" :connection="mcps.find(connection => connection.id === item.id)!" />
    </template>
    <template #empty>
      No MCP connections yet. <RouterLink to="/mcps" class="text-accent hover:underline">
        Add one in MCPs ›
      </RouterLink>
    </template>
  </AgentSelection>
</template>
