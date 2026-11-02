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
import { listPluginResources } from '../../../../packages/contracts/generated/listPluginResources'
import { resolvePluginResource } from '../../../../packages/contracts/generated/resolvePluginResource'
import { pluginRequest } from '../plugin-operations'
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
    const result = await listPluginResources(pluginRequest, {
      pluginId: props.pluginId,
      instanceId: props.instanceId,
      resourceType: type.value,
      query: query.value,
      page: page.value,
    })
    // Resolve ancestry for presentation only; the server resolves it afresh when saving and using grants.
    const resolved = await Promise.all(result.items.map(async item => ({
      item,
      ancestry: await resolvePluginResource(pluginRequest, {
        pluginId: props.pluginId,
        instanceId: props.instanceId,
        resourceType: item.resourceType,
        id: item.id,
      }),
    })))
    if (asked !== generation)
      return
    for (const { item, ancestry } of resolved) {
      paths.value[key(item)] = [...ancestry.ancestors.map(parent => parent.name), item.name].join(' › ')
      if (!items.value.some(held => key(held) === key(item)))
        items.value.push(item)
    }

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
  selected.value = selected.value.some(held => key(held) === key(item)) ? selected.value.filter(held => key(held) !== key(item)) : [...selected.value, { resourceType: item.resourceType, id: item.id }]
}

watch(() => [props.pluginId, props.instanceId, type.value, query.value], () => {
  generation++
  items.value = []
  page.value = 1
  loading.value = false
  void load()
}, { immediate: true })
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
