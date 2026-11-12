<script setup lang="ts">
import type { PluginManifest } from '../../../../packages/contracts/generated/typescript/PluginManifest'
import type { GrantDraft } from '../plugin-grants'
import { computed, ref } from 'vue'
import {
  contributionKindLabel,
  contributionLabel,
  contributions,
  removeGrant,
  removeGrantContribution,
  replaceGrantAccess,
  resourceTypeLabel,
  selectableResourceTypes,
  staleResourceRestrictions,
} from '../plugin-grants'
import AccessChip from './AccessChip.vue'
import PluginResourcePicker from './PluginResourcePicker.vue'
import UiSegments from './UiSegments.vue'

const props = defineProps<{
  manifest: PluginManifest
  instanceId: string
  instanceName: string
  supplierName: string
}>()
const draft = defineModel<GrantDraft>({ required: true })
const grant = computed(() => draft.value.grants.find(item => item.instanceId === props.instanceId))
const selectedMode = ref(grant.value?.access.type === 'selected')
const mode = computed({
  get: () => grant.value?.access.type ?? (selectedMode.value ? 'selected' : 'none'),
  set: value => setMode(value),
})
const modes = [
  { value: 'none' as const, label: 'No access' },
  { value: 'full' as const, label: 'Full access' },
  { value: 'selected' as const, label: 'Choose contributions' },
]
const label = (item: { kind: typeof available.value[number]['kind'], name: string }) => contributionLabel(props.manifest, item)
// Credentials and instructions are named by their kind already.
const tagged = (item: { kind: typeof available.value[number]['kind'] }) => !['runCredentials', 'instructions'].includes(item.kind)
const accessibleName = (item: typeof available.value[number]) => tagged(item) ? `${item.description} (${contributionKindLabel(item.kind)})` : item.description
const resourceLabel = (resource: { resourceType: string, id: string }) => `${resourceTypeLabel(props.manifest.resources ?? [], resource.resourceType)} · ${resource.id}`
const available = computed(() => contributions(props.manifest))
const stale = computed(() => {
  const access = grant.value?.access
  if (access?.type !== 'selected')
    return []

  const availableKeys = new Set(available.value.map(item => `${item.kind}:${item.name}`))
  return access.contributions.filter(item => !availableKeys.has(`${item.kind}:${item.name}`))
})

function setMode(value: string) {
  selectedMode.value = value === 'selected'
  draft.value = replaceGrantAccess(
    draft.value,
    props.instanceId,
    value === 'full' ? { type: 'full' } : null,
  )
}

function update(item: typeof available.value[number], on: boolean, resources = item.resources) {
  const held = grant.value?.access
  const selected = held?.type === 'selected' ? held.contributions.filter(value => value.kind !== item.kind || value.name !== item.name) : []
  const next = removeGrant(draft.value, props.instanceId)
  if (on)
    selected.push({ kind: item.kind, name: item.name, resources })
  if (selected.length)
    next.grants.push({ instanceId: props.instanceId, access: { type: 'selected', contributions: selected } })
  if ((draft.value.runCredentialInstances.includes(props.instanceId) || (on && item.kind === 'runCredentials')) && selected.some(item => item.kind === 'runCredentials'))
    next.runCredentialInstances.push(props.instanceId)
  draft.value = next
}

function held(item: typeof available.value[number]) {
  return grant.value?.access.type === 'selected' ? grant.value.access.contributions.find(value => value.kind === item.kind && value.name === item.name) : undefined
}

function staleResources(item: typeof available.value[number]) {
  if (!['tool', 'command', 'hook'].includes(item.kind))
    return []
  return staleResourceRestrictions(held(item)?.resources ?? [], item.resourceTypes)
}

function removeStale(item: typeof stale.value[number]) {
  draft.value = removeGrantContribution(draft.value, props.instanceId, item)
}

function removeStaleResource(item: typeof available.value[number], resource: typeof item.resources[number]) {
  const current = held(item)
  if (!current)
    return
  update(item, true, current.resources.filter(value => value.resourceType !== resource.resourceType || value.id !== resource.id))
}

function supplier(on: boolean) {
  const ids = draft.value.runCredentialInstances.filter(id => id !== props.instanceId)
  // The parent removes the previous supplying instance of this package.
  draft.value = { ...draft.value, runCredentialInstances: on ? [...ids, props.instanceId] : ids }
}
</script>

<template>
  <div class="grid w-full gap-3">
    <UiSegments
      v-model="mode"
      :label="`Access to ${instanceName}`"
      :options="modes"
      stretch
    />
    <p v-if="mode === 'full'" class="text-xs text-muted">
      All current and future contributions. Run credentials require the explicit supplier choice below. Shared setup steps are available only with full access.
    </p>
    <div v-if="mode === 'selected'" class="grid gap-3">
      <p class="text-xs text-muted">
        Only checked contributions are granted. New contributions stay off. Nothing is granted until you save.
      </p>
      <div v-if="stale.length" role="alert" class="grid gap-2 rounded-lg border border-warning p-3">
        <p class="text-sm text-warning">
          Some saved grants refer to contributions this plugin no longer provides. Remove each stale grant before saving.
        </p>
        <div v-for="item in stale" :key="`${item.kind}:${item.name}`" class="flex items-start justify-between gap-3">
          <span class="text-sm"><strong>{{ label(item) }}</strong> <span class="text-xs text-muted">({{ contributionKindLabel(item.kind) }})</span><small class="block text-xs text-muted">Saved restrictions: {{ item.resources.length ? item.resources.map(resourceLabel).join(', ') : 'all resources' }}</small></span>
          <button
            type="button"
            class="shrink-0 rounded-md border border-line px-3 py-1 text-sm"
            :aria-label="`Remove stale grant ${label(item)} (${contributionKindLabel(item.kind)})`"
            @click="removeStale(item)"
          >
            Remove grant
          </button>
        </div>
      </div>
      <div v-for="item in available" :key="`${item.kind}:${item.name}`" class="grid gap-2 rounded-lg border border-line p-3">
        <label class="flex flex-row! items-start gap-2! text-sm font-normal!">
          <input
            type="checkbox"
            class="mt-1 size-4!"
            :checked="!!held(item)"
            :aria-label="accessibleName(item)"
            @change="update(item, ($event.target as HTMLInputElement).checked)"
          >
          <span class="flex min-w-0 flex-wrap items-center gap-2"><strong class="font-semibold [overflow-wrap:anywhere]">{{ item.description }}</strong><AccessChip v-if="tagged(item)" tone="none">{{ contributionKindLabel(item.kind) }}</AccessChip></span>
        </label>
        <div v-if="staleResources(item).length" role="alert" class="grid gap-2 rounded-lg border border-warning p-3">
          <p class="text-sm text-warning">
            Saved resource roots include types this contribution no longer accepts. Requests fail closed until corrected. Removing the last root makes this contribution unrestricted; uncheck the contribution to revoke it instead.
          </p>
          <div v-for="resource in staleResources(item)" :key="`${resource.resourceType}:${resource.id}`" class="flex min-w-0 flex-col items-start gap-2 sm:flex-row sm:justify-between">
            <span class="min-w-0 break-words text-sm">{{ resourceLabel(resource) }}</span>
            <button
              type="button"
              class="max-w-full whitespace-normal rounded-md border border-line px-3 py-1 text-left text-sm sm:shrink-0"
              :aria-label="`Remove stale resource restriction ${item.description} ${resourceLabel(resource)}`"
              @click="removeStaleResource(item, resource)"
            >
              Remove restriction
            </button>
          </div>
        </div>
        <PluginResourcePicker
          v-if="held(item) && ['tool', 'command', 'hook'].includes(item.kind) && item.resourceTypes.length"
          :model-value="held(item)!.resources"
          :plugin-id="manifest.id"
          :instance-id="instanceId"
          :types="selectableResourceTypes(manifest.resources ?? [], item.resourceTypes)"
          @update:model-value="update(item, true, $event)"
        />
        <p v-else-if="held(item) && staleResources(item).length" class="text-xs text-warning">
          This grant has stale resource restrictions and fails closed. Remove the contribution to revoke its access.
        </p>
        <p v-else-if="held(item)" class="text-xs text-muted">
          {{ item.kind === 'condition' ? 'No resource restriction. Waiting remains limited by project access.' : 'No resource restriction.' }}
        </p>
      </div>
    </div>
    <template v-if="manifest.runCredentials">
      <p class="rounded-lg bg-warning-surface p-3 text-xs text-warning">
        Run credentials give direct access to everything this service account can do and void tool resource restrictions. One instance per plugin package supplies them. Current supplier: {{ supplierName || 'None' }}.
      </p>
      <label v-if="mode === 'full'" class="flex flex-row! items-start gap-2! text-sm font-normal!">
        <input
          type="checkbox"
          class="mt-1 size-4!"
          :checked="draft.runCredentialInstances.includes(instanceId)"
          @change="supplier(($event.target as HTMLInputElement).checked)"
        >
        Supply run credentials from {{ instanceName }}
      </label>
      <p class="text-xs text-muted">
        Removing this supplier leaves no credentials until you choose another instance. Full access to other instances does not supply credentials.
      </p>
    </template>
  </div>
</template>
