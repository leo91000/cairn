<script setup lang="ts">
import type { McpView } from '../../../../packages/contracts/mcp'
import type { AgentAccess } from '../agent-access'
import { computed } from 'vue'

// One connection's tools for an agent: all its enabled tools, or a chosen few.
const props = defineProps<{ connection: McpView }>()
const access = defineModel<AgentAccess>('access', { required: true })
const enabled = computed(() => props.connection.tools.filter(tool => props.connection.enabledTools === null || props.connection.enabledTools.includes(tool.name)))
const chosen = computed(() => access.value.mcpTools[props.connection.id])

function allTools(on: boolean) {
  const tools = { ...access.value.mcpTools }
  if (on)
    delete tools[props.connection.id]
  else
    tools[props.connection.id] = []
  access.value.mcpTools = tools
}

function toggleTool(tool: string, on: boolean) {
  const kept = (chosen.value ?? []).filter(name => name !== tool)
  access.value.mcpTools = { ...access.value.mcpTools, [props.connection.id]: on ? [...kept, tool] : kept }
}
</script>

<template>
  <details class="mx-4 mb-3 ml-11 rounded-lg border border-line bg-inset px-3.5 py-2 text-xs">
    <summary class="flex min-h-9 cursor-pointer items-center font-semibold phone:min-h-11">
      Tools · {{ enabled.length }} enabled on the connection · {{ chosen ? `${chosen.length} chosen` : 'all enabled tools' }}
    </summary>
    <label class="my-2 flex flex-row! items-center gap-2! font-normal! text-ink!">
      <input
        type="checkbox"
        class="size-4 w-4!"
        :checked="!chosen"
        @change="allTools(($event.target as HTMLInputElement).checked)"
      >All enabled tools
    </label>
    <template v-if="chosen">
      <label v-for="tool in enabled" :key="tool.name" class="my-2 ml-6 flex flex-row! items-center gap-2! font-normal! text-ink!">
        <input
          type="checkbox"
          class="size-4 w-4!"
          :checked="chosen.includes(tool.name)"
          @change="toggleTool(tool.name, ($event.target as HTMLInputElement).checked)"
        >{{ tool.name }}
      </label>
      <p v-if="!enabled.length" class="ml-6 text-muted">
        Test this connection in MCPs to discover its tools.
      </p>
    </template>
  </details>
</template>
