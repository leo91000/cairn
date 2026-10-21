<script setup lang="ts">
import type { AgentPluginDirectory } from '../agent-plugin-directory'
import type {
  Manifest,
  PluginPage,
  PluginResource,
  PluginResourceGrant,
} from '../plugins'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { agentPluginManifest } from '../agent-plugin-directory'
import { api } from '../api'
import PluginResourceGrants from './PluginResourceGrants.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

interface Rule extends PluginResourceGrant {
  connectionId: string
  pluginId: string | null
  collection: string | null
  instanceName: string | null
  resourceName: string | null
}

const props = defineProps<{ agentId: string, catalog: AgentPluginDirectory }>()
const emit = defineEmits<{ changed: [] }>()
const rows = ref<Rule[]>([])
const next = ref<string | null>()
const ready = ref(false)
const busy = ref(false)
const writing = ref(false)
const error = ref('')
const removing = ref<Rule>()
let disposed = false
const editorActive = ref(false)
let generation = 0
let reader: AbortController | undefined
const instance = ref('')
const collection = ref('')
const selected = ref('')
const resources = ref<Pick<PluginResource, 'id' | 'name'>[]>([])
const resourcesNext = ref<string | null>()
const resourcesBusy = ref(false)
const resourceError = ref('')
let resourceGeneration = 0
let resourceReader: AbortController | undefined
const candidates = computed(() => props.catalog.connections.filter(connection => props.catalog.packages.some(entry => entry.id === connection.pluginId && entry.active && entry.operations)))
const instanceOptions = computed(() => {
  const names = new Map<string, number>()
  const groups = new Map<string, number>()
  const packages = new Map(props.catalog.packages.map(entry => [entry.id, entry.name]))
  for (const item of candidates.value) {
    names.set(item.name, (names.get(item.name) ?? 0) + 1)
    const key = JSON.stringify([item.pluginId, item.name])
    groups.set(key, (groups.get(key) ?? 0) + 1)
  }

  return candidates.value.map((item) => {
    let label = item.name
    if ((names.get(item.name) ?? 0) > 1) {
      if ((groups.get(JSON.stringify([item.pluginId, item.name])) ?? 0) > 1)
        label += ` · ${item.id}`
      label += ` · ${item.pluginId} (${packages.get(item.pluginId) ?? item.pluginId})`
    }

    return { value: item.id, label }
  })
})
const connection = computed(() => candidates.value.find(item => item.id === instance.value))
const manifest = ref<Manifest>()
const schemaBusy = ref(false)
const schemaAttempt = ref(0)
const schemaError = ref('')
let schemaReader: AbortController | undefined
let schemaGeneration = 0
const collections = computed(() => manifest.value?.resources?.filter(item => manifest.value?.operations?.some(operation => operation.collection === item.name)) ?? [])
const operations = computed(() => manifest.value?.operations?.filter(operation => operation.collection === collection.value) ?? [])
const resourcePath = computed(() => connection.value && collection.value ? `/plugins/${encodeURIComponent(connection.value.pluginId)}/connections/${encodeURIComponent(connection.value.id)}/resources/${encodeURIComponent(collection.value)}` : '')
const resource = computed(() => resources.value.find(item => item.id === selected.value))

async function load(more = false) {
  if (disposed || writing.value || (more && !next.value))
    return
  const ask = ++generation
  reader?.abort()
  const controller = new AbortController()
  reader = controller
  busy.value = true
  ready.value = false
  error.value = ''
  try {
    const cursor = more ? `?cursor=${encodeURIComponent(next.value!)}` : ''
    const result = await api<PluginPage<Rule>>(`/agents/${encodeURIComponent(props.agentId)}/plugin-resource-grants${cursor}`, { signal: controller.signal })
    if (ask !== generation)
      return
    rows.value = more ? [...rows.value.filter(row => !result.items.some(item => item.id === row.id)), ...result.items] : result.items
    next.value = result.nextCursor
    ready.value = true
    emit('changed')
  }
  catch (e) {
    if (ask === generation)
      error.value = (e as Error).message
  }
  finally {
    if (ask === generation)
      busy.value = false
  }
}

async function pageResources(more = false) {
  if (disposed || !resourcePath.value || writing.value || (more && !resourcesNext.value))
    return
  const ask = ++resourceGeneration
  resourceReader?.abort()
  const controller = new AbortController()
  resourceReader = controller
  resourcesBusy.value = true
  resourceError.value = ''
  try {
    const cursor = more ? `?cursor=${encodeURIComponent(resourcesNext.value!)}` : ''
    const listPath = props.catalog.legacyPackages ? resourcePath.value : resourcePath.value.replace('/resources/', '/resource-choices/')
    const result = await api<PluginPage<Pick<PluginResource, 'id' | 'name'>>>(`${listPath}${cursor}`, { signal: controller.signal })
    if (ask !== resourceGeneration)
      return
    resources.value = more ? [...resources.value.filter(row => !result.items.some(item => item.id === row.id)), ...result.items] : result.items
    resourcesNext.value = result.nextCursor
  }
  catch (e) {
    if (ask === resourceGeneration)
      resourceError.value = (e as Error).message
  }
  finally {
    if (ask === resourceGeneration)
      resourcesBusy.value = false
  }
}

async function revoke() {
  if (!removing.value || !ready.value || busy.value)
    return
  writing.value = true
  ready.value = false
  error.value = ''
  try {
    await api(`/agents/${encodeURIComponent(props.agentId)}/plugin-resource-grants/${encodeURIComponent(removing.value.id)}`, { method: 'DELETE' })
    removing.value = undefined
  }
  catch (e) {
    error.value = `${(e as Error).message} Refresh saved rules before making another change.`
    removing.value = undefined
    return
  }
  finally {
    writing.value = false
  }

  await load()
}

watch(() => props.agentId, () => {
  rows.value = []
  void load()
}, { immediate: true })
watch([instance, schemaAttempt, () => {
  const entry = props.catalog.packages.find(item => item.id === connection.value?.pluginId)
  return JSON.stringify([entry?.id, entry?.active, entry?.status, Boolean(props.catalog.legacyPackages)])
}], async () => {
  const current = ++schemaGeneration
  schemaReader?.abort()
  manifest.value = undefined
  collection.value = ''
  schemaError.value = ''
  const id = connection.value?.pluginId
  schemaBusy.value = Boolean(id)
  if (!id)
    return
  const controller = new AbortController()
  schemaReader = controller
  try {
    const value = await agentPluginManifest(props.catalog, id, controller.signal)
    if (current === schemaGeneration)
      manifest.value = value
  }
  catch (error) {
    if (current === schemaGeneration)
      schemaError.value = (error as Error).message
  }
  finally {
    if (current === schemaGeneration)
      schemaBusy.value = false
  }
})
watch(resourcePath, () => {
  resourceGeneration++
  resourceReader?.abort()
  resources.value = []
  resourcesNext.value = undefined
  selected.value = ''
  if (resourcePath.value)
    void pageResources()
})
onBeforeUnmount(() => {
  disposed = true
  generation++
  resourceGeneration++
  reader?.abort()
  resourceReader?.abort()
  schemaGeneration++
  schemaReader?.abort()
})
</script>

<template>
  <section class="grid min-w-0 gap-3 rounded-xl border border-line bg-surface p-5" aria-label="Resource operation access">
    <h3 class="text-sm! font-semibold">
      Resource operation access
    </h3>
    <p class="text-xs text-muted">
      These saved rules are separate from instance-wide access. Profiles, exceptions and scope limits remain on each rule. Availability also depends on the resource, package, provider and requester. Changes here save immediately.
    </p>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <UiButton size="small" :disabled="busy || writing || editorActive" @click="load()">
      {{ busy ? 'Loading saved resource rules…' : 'Refresh saved resource rules' }}
    </UiButton>
    <p v-if="ready && !rows.length" class="text-xs text-muted">
      No saved resource access rules.
    </p>
    <article
      v-for="rule in rows"
      :key="rule.id"
      class="grid gap-2 rounded-lg border border-line p-3"
      aria-label="Saved resource access rule"
    >
      <b class="break-words text-xs">{{ rule.instanceName || 'Removed instance' }} · {{ rule.resourceName || 'Removed resource' }}</b>
      <p class="break-words text-xs">
        {{ rule.profile || 'Selected operations' }} · Allowed explicitly: {{ rule.operations.join(', ') || 'None' }} · Denied in this rule: {{ rule.deniedOperations.join(', ') || 'None' }}
      </p>
      <p v-for="(values, scope) in rule.scopes" :key="scope" class="break-words text-xs">
        {{ scope }}: {{ values.join(', ') }}
      </p>
      <p v-if="!Object.keys(rule.scopes).length" class="text-xs text-muted">
        No additional scope limits.
      </p>
      <UiButton size="small" :disabled="!ready || busy || writing || editorActive" @click="removing = rule">
        Revoke resource rule
      </UiButton>
      <div v-if="removing?.id === rule.id" class="flex flex-wrap gap-2" role="alert">
        <span class="text-xs">Revoke this rule? Other rules retain their permissions.</span>
        <UiButton size="small" :disabled="writing" @click="removing = undefined">
          Cancel revoke
        </UiButton>
        <UiButton size="small" :disabled="writing" @click="revoke">
          Confirm revoke
        </UiButton>
      </div>
    </article>
    <p v-if="next" class="text-xs text-muted">
      More saved rules are available.
    </p>
    <UiButton
      v-if="next"
      size="small"
      :disabled="busy || writing || editorActive"
      @click="load(true)"
    >
      More saved resource rules
    </UiButton>
    <VirtualSelect
      v-model="instance"
      label="Resource app instance"
      :options="instanceOptions"
      :disabled="busy || writing || editorActive"
    />
    <UiAlert v-if="schemaError">
      {{ schemaError }}
    </UiAlert>
    <UiButton
      v-if="schemaError"
      size="small"
      :disabled="schemaBusy || writing || editorActive"
      @click="schemaAttempt++"
    >
      Retry permission schema
    </UiButton>
    <p v-if="schemaBusy" class="text-xs text-muted">
      Loading permission schema…
    </p>
    <VirtualSelect
      v-if="manifest"
      v-model="collection"
      label="Resource collection"
      :options="collections.map(item => ({ value: item.name, label: item.title }))"
      :disabled="busy || writing || editorActive"
    />
    <UiAlert v-if="resourceError">
      {{ resourceError }}
    </UiAlert>
    <UiButton
      v-if="resourcePath"
      size="small"
      :disabled="resourcesBusy || writing || editorActive"
      @click="pageResources()"
    >
      Refresh available resources
    </UiButton>
    <VirtualSelect
      v-if="resourcePath"
      v-model="selected"
      label="Resource for agent access"
      :options="resources.map(item => ({ value: item.id, label: item.name }))"
      :disabled="resourcesBusy || writing || editorActive"
    />
    <UiButton
      v-if="resourcesNext"
      size="small"
      :disabled="resourcesBusy || writing || editorActive"
      @click="pageResources(true)"
    >
      More available resources
    </UiButton>
    <p v-if="resourcesNext" class="text-xs text-muted">
      This resource list has more pages.
    </p>
    <p v-if="selected && !resource" class="break-words text-xs text-muted">
      Selected resource: {{ selected }}. Load its page before changing access.
    </p>
    <PluginResourceGrants
      v-if="resource"
      :key="`${resourcePath}/${resource.id}/${agentId}`"
      :path="`${resourcePath}/${encodeURIComponent(resource.id)}/grants`"
      :name="resource.name"
      :operations="operations"
      :fixed-agent="agentId"
      :enabled="!writing"
      @activity="active => { editorActive = active; if (!active) void load() }"
    />
  </section>
</template>
