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
import { canSelectPluginResourceRoot, MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION } from '../plugin-grants'
import { pluginRequest } from '../plugin-operations'
import { listPluginGrantResources, resolvePluginResourcePage, resolvePluginResourcePath } from '../plugin-resource-paths'
import UiButton from './UiButton.vue'

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
const hierarchy = computed(() => props.types.map((item) => {
  const names = [item.name]
  let parent = item.parentType
  while (parent && !names.includes(parent)) {
    names.unshift(parent)
    parent = props.types.find(candidate => candidate.name === parent)?.parentType
  }

  return { name: item.name, label: names.join(' › ') }
}))

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
      {{ selected.length ? 'Only selected roots and their descendants are covered. Each restricted type must match; other types stay unrestricted.' : 'All resources. Choose a resource to restrict this contribution.' }}
    </p>
    <p class="text-xs text-muted" role="status">
      {{ selected.length }}/{{ MAX_PLUGIN_RESOURCE_ROOTS_PER_CONTRIBUTION }} resource roots selected for this contribution. Maximum 100; remove a root to select another.
    </p>
    <ul v-if="selected.length" class="grid gap-1">
      <li v-for="item in selected" :key="key(item)" class="flex flex-wrap items-center gap-2 text-xs">
        <span class="min-w-0 break-all">{{ paths[key(item)] || `${item.resourceType} · ${item.id}` }} · includes descendants</span>
        <UiButton size="small" :aria-label="`Remove resource ${item.id}`" @click="toggle(item)">
          Remove
        </UiButton>
      </li>
    </ul>
    <label>Resource type<select v-model="type"><option v-for="item in hierarchy" :key="item.name" :value="item.name">{{ item.label }}</option></select></label>
    <label>Search resources<input v-model="query" type="search" placeholder="Search by name"></label>
    <p v-if="error" role="alert" class="text-xs text-danger">
      {{ error }} <UiButton size="small" @click="load">
        Retry resources
      </UiButton>
    </p>
    <div class="grid max-h-64 gap-1 overflow-y-auto" :aria-busy="loading">
      <label v-for="item in items" :key="key(item)" class="flex flex-row! items-start gap-2! rounded p-2 text-xs font-normal!">
        <input
          type="checkbox"
          class="mt-0.5 size-4! shrink-0"
          :checked="selected.some(held => key(held) === key(item))"
          :disabled="!paths[key(item)] || !canSelect(item)"
          @change="toggle(item)"
        >
        <span class="min-w-0 break-words">{{ paths[key(item)] || item.name }}</span>
      </label>
      <p v-if="loading" role="status" class="text-xs text-muted">
        Loading resources…
      </p>
      <p v-else-if="!items.length && !error" class="text-xs text-muted">
        No resources found.
      </p>
      <UiButton
        v-if="page !== null && !error"
        size="small"
        :disabled="loading"
        @click="load"
      >
        Load more resources
      </UiButton>
    </div>
  </fieldset>
</template>
