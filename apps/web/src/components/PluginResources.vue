<script setup lang="ts">
import type {
  Manifest,
  PluginPage,
  PluginResource,
  PluginResourceCollection,
} from '../plugins'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { api } from '../api'
import { fieldSettings, fieldValues } from '../plugins'
import PluginFields from './PluginFields.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{
  plugin: string
  manifest: Manifest
  instance: string
  setupRevision?: string
  disabled?: boolean
}>()
const emit = defineEmits<{ state: [value: { busy: boolean, editing: boolean }] }>()
const path = computed(() => `/plugins/${props.plugin}/${props.setupRevision ? 'setup-drafts' : 'connections'}/${props.instance}/resources`)
const pages = ref<Record<string, PluginPage<PluginResource>>>({})
const editing = ref<{ collection: PluginResourceCollection, resource?: PluginResource }>()
const name = ref('')
const parent = ref('')
const enabled = ref(true)
const values = ref<Record<string, any>>({})
const secrets = ref<Record<string, string>>({})
const busy = ref(false)
const error = ref('')
const removing = ref<PluginResource>()
let generation = 0

watch(() => [busy.value, Boolean(editing.value)], () => {
  emit('state', { busy: busy.value, editing: Boolean(editing.value) })
}, { immediate: true, flush: 'sync' })

async function load() {
  const current = ++generation
  try {
    const entries = await Promise.all((props.manifest.resources ?? []).map(async collection => [
      collection.name,
      await api<PluginPage<PluginResource>>(`${path.value}/${collection.name}`),
    ] as const))
    if (current === generation)
      pages.value = Object.fromEntries(entries)
  }
  catch (e) {
    if (current === generation)
      error.value = (e as Error).message
  }
}

async function action(operation: () => Promise<void>) {
  busy.value = true
  error.value = ''
  try {
    await operation()
  }
  catch (e) {
    error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function more(collection: string) {
  await action(async () => {
    const cursor = pages.value[collection]?.nextCursor
    if (!cursor)
      return
    const page = await api<PluginPage<PluginResource>>(`${path.value}/${collection}?cursor=${encodeURIComponent(cursor)}`)
    pages.value[collection] = { items: [...pages.value[collection].items, ...page.items], nextCursor: page.nextCursor }
  })
}

async function edit(collection: PluginResourceCollection, resource?: PluginResource) {
  await action(async () => {
    // Parent choices include later pages too; the native resource limit bounds this.
    if (collection.parent) {
      while (pages.value[collection.parent]?.nextCursor) {
        const page = pages.value[collection.parent]
        const next = await api<PluginPage<PluginResource>>(`${path.value}/${collection.parent}?cursor=${encodeURIComponent(page.nextCursor!)}`)
        pages.value[collection.parent] = { items: [...page.items, ...next.items], nextCursor: next.nextCursor }
      }
    }

    name.value = resource?.name ?? ''
    parent.value = resource?.parentId ?? ''
    enabled.value = resource?.enabled ?? true
    values.value = fieldValues(collection.settings ?? [], resource?.settings ?? {})
    secrets.value = {}
    editing.value = { collection, resource }
  })
}

function close() {
  editing.value = undefined
  secrets.value = {}
}

async function save() {
  if (!editing.value)
    return
  await action(async () => {
    const { collection, resource } = editing.value!
    await api(`${path.value}/${collection.name}${resource ? `/${resource.id}` : ''}`, {
      method: resource ? 'PUT' : 'POST',
      body: JSON.stringify({
        name: name.value,
        settings: fieldSettings(collection.settings ?? [], values.value),
        secrets: Object.fromEntries(Object.entries(secrets.value).filter(([, value]) => value.trim())),
        enabled: enabled.value,
        ...(collection.parent && { parentId: parent.value }),
        ...(resource && { revision: resource.revision }),
        ...(props.setupRevision && { setupRevision: props.setupRevision }),
      }),
    })
    close()
    await load()
  })
}

async function remove() {
  await action(async () => {
    const resource = removing.value!
    await api(`${path.value}/${resource.collection}/${resource.id}`, {
      method: 'DELETE',
      body: JSON.stringify({ revision: resource.revision, ...(props.setupRevision && { setupRevision: props.setupRevision }) }),
    })
    removing.value = undefined
    await load()
  })
}

watch(path, () => {
  pages.value = {}
  void load()
}, { immediate: true })
onBeforeUnmount(() => {
  generation++
  secrets.value = {}
  emit('state', { busy: false, editing: false })
})
</script>

<template>
  <div class="grid min-w-0 gap-5" aria-label="Resources">
    <UiButton size="small" :disabled="busy" @click="load">
      Refresh resources
    </UiButton>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <section
      v-for="collection in manifest.resources"
      :key="collection.name"
      class="grid min-w-0 gap-3"
      :aria-label="collection.title"
    >
      <header class="flex flex-wrap items-center justify-between gap-2">
        <h3 class="text-sm! font-bold">
          {{ collection.title }}
        </h3>
        <UiButton size="small" :disabled="busy || disabled" @click="edit(collection)">
          Add {{ collection.singular.toLowerCase() }}
        </UiButton>
      </header>
      <p v-if="pages[collection.name]?.items.length === 0" class="text-xs text-muted">
        None yet.
      </p>
      <article
        v-for="resource in pages[collection.name]?.items"
        :key="resource.id"
        class="grid min-w-0 gap-2 rounded-xl border border-line p-3"
        :aria-label="resource.name"
      >
        <div class="flex flex-wrap items-center justify-between gap-2">
          <b class="break-words text-sm">{{ resource.name }}</b>
          <span class="text-xs text-muted">{{ setupRevision ? 'Prepared' : resource.enabled ? 'Enabled' : 'Paused' }}</span>
        </div>
        <p v-if="resource.parentId" class="text-xs text-muted">
          {{ pages[collection.parent ?? '']?.items.find(item => item.id === resource.parentId)?.name ?? 'Parent on another page' }}
        </p>
        <div class="flex gap-2">
          <UiButton
            size="small"
            :disabled="busy || disabled"
            :aria-label="`Edit ${resource.name}`"
            @click="edit(collection, resource)"
          >
            Edit
          </UiButton>
          <UiButton
            size="small"
            :disabled="busy || disabled"
            :aria-label="`Delete ${resource.name}`"
            @click="removing = resource"
          >
            Delete
          </UiButton>
        </div>
        <div v-if="removing?.id === resource.id" class="grid gap-2 rounded-lg bg-inset p-3" role="alert">
          <p class="text-xs">
            Delete {{ resource.name }} and its child resources? Their saved credentials and access rules will be removed.
          </p>
          <div class="flex gap-2">
            <UiButton size="small" :disabled="busy" @click="removing = undefined">
              Cancel
            </UiButton>
            <UiButton size="small" :disabled="busy || disabled" @click="remove">
              Confirm delete
            </UiButton>
          </div>
        </div>
      </article>
      <UiButton
        v-if="pages[collection.name]?.nextCursor"
        size="small"
        :disabled="busy"
        @click="more(collection.name)"
      >
        More {{ collection.title.toLowerCase() }}
      </UiButton>
      <form
        v-if="editing?.collection.name === collection.name"
        class="grid min-w-0 gap-4 rounded-xl border border-line bg-inset p-4"
        :aria-label="editing.resource ? `Edit ${editing.resource.name}` : `New ${collection.singular.toLowerCase()}`"
        @submit.prevent="save"
      >
        <label>Name<input
          v-model="name"
          :disabled="busy || disabled"
          required
          maxlength="100"
        ></label>
        <label v-if="collection.parent">Parent<select v-model="parent" :disabled="busy || disabled" required>
          <option value="">Choose a parent</option>
          <option v-for="item in pages[collection.parent]?.items" :key="item.id" :value="item.id">{{ item.name }}</option>
        </select></label>
        <PluginFields
          v-model:values="values"
          v-model:secrets="secrets"
          :fields="(collection.settings ?? []).map(field => setupRevision ? { ...field, required: false } : field)"
          :saved="editing.resource?.secretKeys"
          :disabled="busy || disabled"
        />
        <label class="flex flex-row! items-center gap-2!"><input
          v-model="enabled"
          type="checkbox"
          class="w-auto!"
          :disabled="busy || disabled"
        >Enabled</label>
        <div class="flex gap-2">
          <UiButton type="button" :disabled="busy" @click="close">
            Cancel
          </UiButton>
          <UiButton type="submit" variant="primary" :disabled="busy || disabled">
            Save {{ collection.singular.toLowerCase() }}
          </UiButton>
        </div>
      </form>
    </section>
  </div>
</template>
