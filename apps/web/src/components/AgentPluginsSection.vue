<script setup lang="ts">
import type { AgentPluginInstance, AgentPluginPackage } from '../agent-plugin-directory'
import { computed, ref } from 'vue'
import { grantablePackages, instanceState } from '../agent-access'
import {
  ArrowUpRight,
  Search,
  TriangleAlert,
  X,
} from '../icons'
import AccessChip from './AccessChip.vue'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import UiSwitch from './UiSwitch.vue'

// Plugin instances whose package gives agents something, grouped by package, and the
// delegations (e.g. a Discord bot) that answer as this agent, which are set on the instance.
const props = defineProps<{
  agentId: string | null
  packages: AgentPluginPackage[]
  connections: AgentPluginInstance[]
  // Delegations currently stopped until their new access is reapproved.
  suspended: string[]
  // Why grants cannot be changed here, e.g. on an older installation.
  readOnlyGrants?: string
}>()
// Granted plugin instance ids.
const plugins = defineModel<string[]>('plugins', { required: true })
const query = ref('')
const grantable = computed(() => grantablePackages(props.packages))
const instances = computed(() => props.connections.filter(connection => grantable.value.some(entry => entry.id === connection.pluginId)))
const searchable = computed(() => instances.value.length >= 8)
const groups = computed(() => {
  const words = query.value.trim().toLowerCase()
  return grantable.value
    .map(entry => ({
      entry,
      instances: instances.value.filter(connection => connection.pluginId === entry.id
        && (!words || `${connection.name} ${entry.name}`.toLowerCase().includes(words))),
    }))
    .filter(group => group.instances.length || !words)
})
const delegations = computed(() => props.agentId ? props.connections.filter(connection => connection.agentId === props.agentId) : [])
const missing = computed(() => plugins.value.filter(id => !props.connections.some(connection => connection.id === id)))
// Held grants on instances whose package is invalid, awaits review or no longer gives agents access.
const unusable = computed(() => props.connections.filter(connection => plugins.value.includes(connection.id) && !grantable.value.some(entry => entry.id === connection.pluginId)))

function packageName(pluginId: string) {
  return props.packages.find(entry => entry.id === pluginId)?.name ?? pluginId
}

function set(id: string, on: boolean) {
  const granted = plugins.value.filter(value => value !== id)
  plugins.value = on ? [...granted, id] : granted
}
</script>

<template>
  <div class="grid min-w-0 gap-5">
    <p v-if="readOnlyGrants" class="rounded-xl bg-soft px-5 py-3 text-sm" role="note">
      {{ readOnlyGrants }}
    </p>
    <label v-if="searchable" class="search-field flex min-w-0 flex-row items-center gap-[7px] rounded-[7px] border border-line bg-raised px-2.5 py-0 text-subtle"><Icon :name="Search" :size="16" /><input
      v-model="query"
      type="search"
      placeholder="Search an account or a plugin"
      aria-label="Search plugin instances"
    ></label>

    <section
      v-for="{ entry, instances: rows } in groups"
      :key="entry.id"
      class="min-w-0 overflow-hidden rounded-xl border border-line bg-surface"
      :aria-label="entry.name"
    >
      <header class="flex flex-wrap items-baseline gap-x-3 gap-y-1 border-b border-line px-5 py-3.5 phone:px-4">
        <h3 class="text-sm! font-semibold">
          {{ entry.name }}
        </h3>
        <p class="text-2xs text-muted">
          {{ 'Commands, tools and instructions from each instance' }}
        </p>
      </header>
      <ul v-if="rows.length" class="divide-y divide-line">
        <li v-for="connection in rows" :key="connection.id" class="flex min-w-0 items-center gap-3 px-5 py-3 phone:flex-wrap phone:px-4">
          <UiSwitch
            :model-value="plugins.includes(connection.id)"
            :disabled="Boolean(readOnlyGrants)"
            :label="`Access to ${packageName(connection.pluginId)} ${connection.name}`"
            @update:model-value="set(connection.id, $event)"
          />
          <div class="min-w-0 flex-1">
            <p class="flex flex-wrap items-center gap-2 text-sm">
              <span class="min-w-0 truncate font-semibold">{{ connection.name }}</span>
              <AccessChip v-if="!plugins.includes(connection.id)" tone="none">
                Not granted
              </AccessChip>
              <AccessChip v-else :tone="instanceState(connection).tone">
                {{ instanceState(connection).label }}
              </AccessChip>
            </p>
            <p v-if="plugins.includes(connection.id) && !connection.enabled" class="text-2xs text-muted">
              The access is kept, but the agent cannot use it while it stays disabled.
            </p>
            <p v-else-if="connection.status.message" class="truncate text-2xs text-muted">
              {{ connection.status.message }}
            </p>
          </div>
          <RouterLink :to="`/plugins/${connection.pluginId}`" class="inline-flex min-h-11 items-center gap-1 text-xs font-medium text-accent hover:underline phone:ml-auto">
            {{ 'Configure' }}<Icon :name="ArrowUpRight" :size="13" />
          </RouterLink>
        </li>
      </ul>
      <p v-else class="px-5 py-4 text-sm text-muted phone:px-4">
        No {{ entry.instances?.plural.toLowerCase() ?? 'instances' }} yet. <RouterLink :to="`/plugins/${entry.id}`" class="text-accent hover:underline">
          Add one ›
        </RouterLink>
      </p>
      <p class="mx-5 mb-4 rounded-lg bg-soft px-4 py-3 text-xs leading-relaxed phone:mx-4" :class="rows.length ? '' : 'mt-0'">
        {{ entry.agentAccessDescription || 'Agents with access receive this instance’s commands, tools and instructions.' }}
      </p>
    </section>

    <p v-if="!grantable.length" class="rounded-xl border border-dashed border-line p-5 text-sm text-muted">
      No installed plugin gives agents access yet. <RouterLink to="/plugins" class="text-accent hover:underline">
        Browse plugins ›
      </RouterLink>
    </p>

    <section v-if="unusable.length" class="min-w-0 overflow-hidden rounded-xl border border-line bg-surface" aria-label="Unavailable plugin access">
      <header class="border-b border-line px-5 py-3.5 phone:px-4">
        <h3 class="text-sm! font-semibold">
          Unavailable access
        </h3>
        <p class="text-2xs text-muted">
          Granted instances whose plugin needs review or no longer gives agents access. The agent cannot use them.
        </p>
      </header>
      <ul class="divide-y divide-line">
        <li v-for="connection in unusable" :key="connection.id" class="flex min-w-0 items-center gap-3 px-5 py-3 phone:flex-wrap phone:px-4">
          <div class="min-w-0 flex-1">
            <p class="flex flex-wrap items-center gap-2 text-sm">
              <span class="min-w-0 truncate font-semibold">{{ packageName(connection.pluginId) }} · {{ connection.name }}</span>
              <AccessChip tone="unavailable">
                Unavailable
              </AccessChip>
            </p>
            <p class="text-2xs text-muted">
              Its plugin needs review or no longer gives agents access. The access is kept, but the agent cannot use it.
            </p>
          </div>
          <RouterLink :to="`/plugins/${connection.pluginId}`" class="inline-flex min-h-11 items-center gap-1 text-xs font-medium text-accent hover:underline">
            Review<Icon :name="ArrowUpRight" :size="13" />
          </RouterLink>
          <UiButton
            v-if="!readOnlyGrants"
            size="small"
            :aria-label="`Remove access to ${packageName(connection.pluginId)} ${connection.name}`"
            @click="set(connection.id, false)"
          >
            <Icon :name="X" :size="13" />Remove
          </UiButton>
        </li>
      </ul>
    </section>

    <div v-if="missing.length" class="flex flex-wrap items-center gap-3 rounded-xl border border-line px-5 py-3 text-sm">
      <span class="min-w-0 flex-1 text-muted">{{ missing.length === 1 ? 'A granted instance was deleted.' : `${missing.length} granted instances were deleted.` }}</span>
      <AccessChip tone="missing">
        Not found
      </AccessChip>
      <UiButton
        v-if="!readOnlyGrants"
        size="small"
        aria-label="Remove deleted plugin instances"
        @click="plugins = plugins.filter(id => !missing.includes(id))"
      >
        <Icon :name="X" :size="13" />Remove
      </UiButton>
    </div>

    <section v-if="delegations.length" class="min-w-0 overflow-hidden rounded-xl border border-line bg-surface" aria-label="Answers as this agent">
      <header class="border-b border-line px-5 py-3.5 phone:px-4">
        <h3 class="text-sm! font-semibold">
          Answers as this agent
        </h3>
        <p class="text-2xs text-muted">
          External people talk to this agent through these instances.
        </p>
      </header>
      <ul class="divide-y divide-line">
        <li v-for="connection in delegations" :key="connection.id" class="flex min-w-0 items-start gap-3 px-5 py-3 phone:flex-wrap phone:px-4">
          <div class="min-w-0 flex-1">
            <p class="flex flex-wrap items-center gap-2 text-sm">
              <span class="min-w-0 font-semibold">{{ packageName(connection.pluginId) }} · Answers as this agent on {{ connection.name }}</span>
              <AccessChip tone="fixed">
                Set on the instance
              </AccessChip>
              <AccessChip v-if="suspended.includes(connection.id)" tone="unavailable">
                Suspended until reapproved
              </AccessChip>
            </p>
            <p class="mt-1 flex items-start gap-1.5 text-2xs text-muted">
              <Icon :name="TriangleAlert" :size="13" class="mt-px shrink-0 text-warning" />Changing this agent’s access suspends the bot until it is reapproved (you can reapprove when saving).
            </p>
          </div>
          <RouterLink :to="`/plugins/${connection.pluginId}`" class="inline-flex min-h-11 items-center gap-1 text-xs font-medium text-accent hover:underline phone:ml-auto">
            Open<Icon :name="ArrowUpRight" :size="13" />
          </RouterLink>
        </li>
      </ul>
    </section>
  </div>
</template>
