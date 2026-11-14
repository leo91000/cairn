<script setup lang="ts">
import type { PluginResource } from '../../../../packages/contracts/generated/typescript/PluginResource'
import type { PluginResourceRef } from '../../../../packages/contracts/generated/typescript/PluginResourceRef'
import { onBeforeUnmount, ref, watch } from 'vue'
import { listPluginResources } from '../../../../packages/contracts/generated/listPluginResources'
import { resolvePluginResource } from '../../../../packages/contracts/generated/resolvePluginResource'
import { Search, X } from '../icons'
import { pluginRequest } from '../plugin-operations'
import Icon from './Icon.vue'

const props = defineProps<{
  pluginId: string
  instanceId: string
  resourceType: string
  label: string
  multiple?: boolean
}>()
const model = defineModel<PluginResourceRef[]>({ required: true })
const open = ref(false)
const query = ref('')
const items = ref<PluginResource[]>([])
const names = ref<Record<string, string>>({})
const next = ref<number | null>(1)
const loading = ref(false)
const error = ref('')
let generation = 0
let timer: ReturnType<typeof setTimeout> | undefined
let disposed = false
const key = (item: PluginResourceRef) => `${item.resourceType}:${item.id}`

async function load(more = false) {
  const asked = generation
  const page = more ? next.value : 1
  if (!page)
    return
  loading.value = true
  error.value = ''
  try {
    const result = await listPluginResources(pluginRequest, {
      pluginId: props.pluginId,
      instanceId: props.instanceId,
      resourceType: props.resourceType,
      query: query.value,
      page,
    })
    if (disposed || asked !== generation)
      return
    items.value = more ? [...items.value, ...result.items.filter(item => !items.value.some(held => key(item) === key(held)))] : result.items
    for (const item of result.items)
      names.value[key(item)] = item.name
    next.value = result.nextPage
  }
  catch (e) {
    if (!disposed && asked === generation)
      error.value = (e as Error).message
  }
  finally {
    if (!disposed && asked === generation)
      loading.value = false
  }
}

function choose(item: PluginResourceRef) {
  const held = model.value.some(selected => key(selected) === key(item))
  model.value = held ? model.value.filter(selected => key(selected) !== key(item)) : props.multiple ? [...model.value, { resourceType: item.resourceType, id: item.id }] : [{ resourceType: item.resourceType, id: item.id }]
  if (!props.multiple)
    open.value = false
}

watch(() => [props.pluginId, props.instanceId, props.resourceType, query.value, open.value], () => {
  generation++
  clearTimeout(timer)
  if (open.value)
    timer = setTimeout(() => void load(), 200)
})
watch(() => [props.pluginId, props.instanceId, model.value] as const, async () => {
  const askedInstance = props.instanceId
  for (const selected of model.value) {
    if (names.value[key(selected)])
      continue
    try {
      const result = await resolvePluginResource(pluginRequest, {
        pluginId: props.pluginId,
        instanceId: props.instanceId,
        resourceType: selected.resourceType,
        id: selected.id,
      })
      if (!disposed && props.instanceId === askedInstance)
        names.value[key(selected)] = result.resource.name
    }
    catch { /* Existing selections remain visible if their service is unavailable. */ }
  }
}, { immediate: true, deep: true })
onBeforeUnmount(() => {
  disposed = true
  generation++
  clearTimeout(timer)
})
</script>

<template>
  <div class="grid gap-2">
    <div class="flex flex-wrap gap-1.5">
      <span v-for="selected in model" :key="key(selected)" class="inline-flex items-center gap-1 rounded-full bg-accent/10 py-1 pr-1 pl-3 text-xs font-semibold text-accent">
        {{ names[key(selected)] || 'Selected resource' }}
        <button
          type="button"
          class="grid size-6 place-items-center rounded-full hover:bg-accent/15"
          :aria-label="`Remove ${names[key(selected)] || 'selection'}`"
          @click="choose(selected)"
        ><Icon :name="X" :size="12" /></button>
      </span>
    </div>
    <button
      type="button"
      class="justify-self-start rounded-full border border-accent/30 px-3 py-1.5 text-xs font-semibold text-accent hover:bg-accent/8"
      :aria-expanded="open"
      @click="open = !open"
    >
      {{ label }}
    </button>
    <section v-if="open" class="grid gap-2 rounded-2xl border border-line bg-raised p-3" :aria-label="label">
      <label class="flex items-center gap-2 rounded-xl border border-line bg-surface px-3">
        <Icon :name="Search" :size="14" class="text-muted" /><input
          v-model="query"
          class="min-w-0 flex-1 border-0! bg-transparent!"
          placeholder="Search by name"
          :aria-label="`Search ${resourceType}`"
        >
      </label>
      <p v-if="error" class="m-0! text-xs text-coral" role="alert">
        {{ error }}
      </p>
      <p v-if="loading" class="m-0! text-xs text-muted" role="status">
        Loading choices…
      </p>
      <div class="max-h-60 overflow-y-auto">
        <button
          v-for="item in items"
          :key="key(item)"
          type="button"
          class="flex w-full items-center gap-2 rounded-lg px-2 py-2 text-left text-sm hover:bg-hover"
          :aria-pressed="model.some(selected => key(selected) === key(item))"
          @click="choose(item)"
        >
          <span class="min-w-0 flex-1 truncate">{{ item.name }}</span><span v-if="model.some(selected => key(selected) === key(item))" class="text-accent">✓</span>
        </button>
        <button
          v-if="next && !loading"
          type="button"
          class="px-2 py-2 text-xs font-semibold text-accent"
          @click="load(true)"
        >
          Load more
        </button>
        <p v-if="!loading && !items.length" class="m-0! px-2 py-3 text-xs text-muted">
          No matching choices.
        </p>
      </div>
    </section>
  </div>
</template>
