<script setup lang="ts">
import type { PluginResource } from '../../../../packages/contracts/generated/typescript/PluginResource'
import type { PluginResourceRef } from '../../../../packages/contracts/generated/typescript/PluginResourceRef'
import type { PluginResourceType } from '../../../../packages/contracts/generated/typescript/PluginResourceType'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import {
  Check,
  Plus,
  Search,
  X,
} from '../icons'
import {
  canSelectPluginResourceRoot,
  MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION,
  resourceTypeLabel,
  resourceTypePathLabel,
} from '../plugin-grants'
import { pluginRequest } from '../plugin-operations'
import { listPluginGrantResources, resolvePluginResourcePage, resolvePluginResourcePath } from '../plugin-resource-paths'
import Icon from './Icon.vue'
import UiButton from './UiButton.vue'
import UiSegments from './UiSegments.vue'

const props = defineProps<{ pluginId: string, instanceId: string, types: PluginResourceType[] }>()
const selected = defineModel<PluginResourceRef[]>({ required: true })
const type = ref(props.types[0]?.name ?? '')
const query = ref('')
const items = ref<PluginResource[]>([])
const paths = ref<Record<string, string>>({})
const page = ref<number | null>(1)
const loading = ref(false)
const error = ref('')
let generation = 0
const key = (item: PluginResourceRef) => `${item.resourceType}:${item.id}`
const hierarchy = computed(() => props.types.map(item => ({ value: item.name, label: resourceTypePathLabel(props.types, item.name) })))
const isSelected = (item: PluginResourceRef) => selected.value.some(held => key(held) === key(item))
const rootLabel = (item: PluginResourceRef) => paths.value[key(item)] || `${resourceTypeLabel(props.types, item.resourceType)} · ${item.id}`

async function load() {
  if (loading.value || page.value === null || !type.value)
    return
  const asked = generation
  loading.value = true
  error.value = ''
  try {
    const result = await listPluginGrantResources(pluginRequest, {
      pluginId: props.pluginId,
      instanceId: props.instanceId,
      resourceType: type.value,
      query: query.value,
      page: page.value,
    })
    if (asked !== generation)
      return
    // Resolve ancestry for presentation only; the server resolves it afresh when saving and using grants.
    const resolved = await resolvePluginResourcePage(pluginRequest, props.pluginId, props.instanceId, result.items)
    if (asked !== generation)
      return
    if (page.value === 1)
      items.value = []
    for (const { item, path } of resolved) {
      if (path !== null)
        paths.value[key(item)] = path
      if (!items.value.some(held => key(held) === key(item)))
        items.value.push(item)
    }

    if (resolved.some(item => item.path === null))
      error.value = 'Some resource paths could not be confirmed. Resolved choices and selected roots are kept; retry to load the remaining paths.'
    else
      page.value = result.nextPage
  }
  catch (e) {
    if (asked === generation)
      error.value = (e as Error).message
  }
  finally {
    if (asked === generation)
      loading.value = false
  }
}

function toggle(item: PluginResourceRef) {
  const alreadySelected = selected.value.some(held => key(held) === key(item))
  if (!canSelectPluginResourceRoot(selected.value.length, alreadySelected))
    return

  selected.value = alreadySelected
    ? selected.value.filter(held => key(held) !== key(item))
    : [...selected.value, { resourceType: item.resourceType, id: item.id }]
}

function canSelect(item: PluginResourceRef) {
  return canSelectPluginResourceRoot(
    selected.value.length,
    selected.value.some(held => key(held) === key(item)),
  )
}

watch(() => [props.pluginId, props.instanceId, type.value, query.value], () => {
  generation++
  items.value = []
  page.value = 1
  loading.value = false
  void load()
}, { immediate: true })
watch(() => [props.pluginId, props.instanceId, selected.value] as const, async ([, , roots]) => {
  const unresolved = roots.filter(root => !paths.value[key(root)])
  if (!unresolved.length)
    return

  const resolved = await Promise.all(unresolved.map(async (root) => {
    try {
      return [key(root), await resolvePluginResourcePath(pluginRequest, props.pluginId, props.instanceId, root)] as const
    }
    catch {
      return null
    }
  }))

  for (const item of resolved) {
    if (item && selected.value.some(root => key(root) === item[0]))
      paths.value[item[0]] = item[1]
  }
}, { deep: true, immediate: true })
onBeforeUnmount(() => generation++)
</script>

<template>
  <fieldset class="grid gap-3 rounded-lg border border-line bg-inset p-3">
    <legend class="text-xs font-semibold">
      Resource restrictions
    </legend>
    <p class="text-xs text-muted">
      {{ selected.length ? 'Only these resources and their contents are covered. Each restricted type must match; other types stay unrestricted.' : 'All resources. Add a resource to restrict this contribution.' }}
    </p>
    <ul v-if="selected.length" class="flex flex-wrap gap-1.5" aria-label="Selected resources">
      <li v-for="item in selected" :key="key(item)" class="inline-flex max-w-full items-center gap-1 rounded-full border border-line bg-surface py-0.5 pr-0.5 pl-2.5 text-xs">
        <span class="min-w-0 truncate" :title="rootLabel(item)">{{ rootLabel(item) }}</span>
        <button
          type="button"
          class="grid size-6 shrink-0 place-items-center rounded-full text-muted hover:bg-hover hover:text-ink focus-visible:outline-2 focus-visible:outline-accent"
          :aria-label="`Remove ${rootLabel(item)}`"
          @click="toggle(item)"
        >
          <Icon :name="X" :size="13" />
        </button>
      </li>
    </ul>
    <p class="text-2xs text-muted" role="status">
      {{ selected.length }} of {{ MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION }} resources chosen{{ selected.length >= MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION ? ' — remove one to add another' : '' }}
    </p>
    <UiSegments
      v-if="hierarchy.length > 1"
      v-model="type"
      label="Resource type"
      :options="hierarchy"
      compact
    />
    <label class="relative block">
      <span class="sr-only">Search resources</span>
      <Icon :name="Search" :size="14" class="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-muted" />
      <input
        v-model="query"
        type="search"
        class="pl-8!"
        :placeholder="`Search ${hierarchy.find(item => item.value === type)?.label ?? 'resources'}`"
        aria-label="Search resources"
      >
    </label>
    <p v-if="error" role="alert" class="text-xs text-danger">
      {{ error }} <UiButton size="small" @click="load">
        Retry resources
      </UiButton>
    </p>
    <ul class="grid max-h-64 gap-0.5 overflow-y-auto" :aria-busy="loading" aria-label="Resources">
      <li v-for="item in items" :key="key(item)" class="flex min-w-0 items-center justify-between gap-2 rounded-md px-2 py-1.5 text-xs hover:bg-hover">
        <span class="min-w-0 break-words">{{ paths[key(item)] || item.name }}</span>
        <button
          type="button"
          class="inline-flex shrink-0 items-center gap-1 rounded-md border px-2 py-1 font-medium disabled:cursor-not-allowed disabled:opacity-50"
          :class="isSelected(item) ? 'border-transparent bg-soft text-ink' : 'border-line bg-surface text-ink hover:bg-hover'"
          :aria-pressed="isSelected(item)"
          :aria-label="`${isSelected(item) ? 'Remove' : 'Add'} ${paths[key(item)] || item.name}`"
          :disabled="!paths[key(item)] || !canSelect(item)"
          @click="toggle(item)"
        >
          <Icon :name="isSelected(item) ? Check : Plus" :size="12" />
          {{ isSelected(item) ? 'Added' : 'Add' }}
        </button>
      </li>
      <li v-if="loading" role="status" class="px-2 text-xs text-muted">
        Loading resources…
      </li>
      <li v-else-if="!items.length && !error" class="px-2 text-xs text-muted">
        No resources found.
      </li>
    </ul>
    <UiButton
      v-if="page !== null && !error"
      size="small"
      class="justify-self-start"
      :disabled="loading"
      @click="load"
    >
      Load more resources
    </UiButton>
  </fieldset>
</template>
