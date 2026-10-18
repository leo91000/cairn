<script setup lang="ts">
import type { McpView } from '../../../../packages/contracts/mcp'
import type { AgentAccess, SelectionItem } from '../agent-access'
import { computed } from 'vue'
import AgentSelection from './AgentSelection.vue'

// MCP connections, and per connection either all its enabled tools or a chosen few.
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
    access.value.mcps = value
    // Tool choices only apply to connections that stay selected.
    access.value.mcpTools = value === null
      ? {}
      : Object.fromEntries(Object.entries(access.value.mcpTools).filter(([id]) => value.includes(id)))
  },
})

function enabledTools(connection: McpView) {
  return connection.tools.filter(tool => connection.enabledTools === null || connection.enabledTools.includes(tool.name))
}

function toolSummary(connection: McpView) {
  const enabled = enabledTools(connection).length
  const chosen = access.value.mcpTools[connection.id]
  return `${enabled} enabled on the connection · ${chosen ? `${chosen.length} chosen` : 'all enabled tools'}`
}

function allTools(id: string, on: boolean) {
  const tools = { ...access.value.mcpTools }
  if (on)
    delete tools[id]
  else
    tools[id] = []
  access.value.mcpTools = tools
}

function toggleTool(id: string, tool: string, on: boolean) {
  const chosen = (access.value.mcpTools[id] ?? []).filter(name => name !== tool)
  access.value.mcpTools = { ...access.value.mcpTools, [id]: on ? [...chosen, tool] : chosen }
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
      <p>Every MCP connection, including future ones, with all of its enabled tools.</p>
    </template>
    <template #item="{ item, selected }">
      <details v-if="selected" class="mx-4 mb-3 ml-11 rounded-lg border border-line bg-inset px-3.5 py-2 text-xs">
        <summary class="flex min-h-9 cursor-pointer items-center font-semibold phone:min-h-11">
          Tools · {{ item.detail }}
        </summary>
        <label class="my-2 flex flex-row! items-center gap-2! font-normal! text-ink!">
          <input
            type="checkbox"
            class="size-4 w-4!"
            :checked="!(item.id in access.mcpTools)"
            @change="allTools(item.id, ($event.target as HTMLInputElement).checked)"
          >All enabled tools
        </label>
        <template v-if="item.id in access.mcpTools">
          <label v-for="tool in enabledTools(mcps.find(connection => connection.id === item.id)!)" :key="tool.name" class="my-2 ml-6 flex flex-row! items-center gap-2! font-normal! text-ink!">
            <input
              type="checkbox"
              class="size-4 w-4!"
              :checked="access.mcpTools[item.id]!.includes(tool.name)"
              @change="toggleTool(item.id, tool.name, ($event.target as HTMLInputElement).checked)"
            >{{ tool.name }}
          </label>
          <p v-if="!enabledTools(mcps.find(connection => connection.id === item.id)!).length" class="ml-6 text-muted">
            Test this connection in MCPs to discover its tools.
          </p>
        </template>
      </details>
    </template>
    <template #empty>
      No MCP connections yet. <RouterLink to="/mcps" class="text-accent hover:underline">
        Add one in MCPs ›
      </RouterLink>
    </template>
  </AgentSelection>
</template>
