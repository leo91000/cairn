<script setup lang="ts">
import type {
  FormResult,
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
import { api, ApiError } from '../api'
import { pluginFormLookup } from '../plugin-form-lookups'
import {
  pluginFormSecrets,
  pluginFormValue,
  pluginFormValues,
  pluginSecrets,
  pluginSettings,
} from '../plugin-form-values'
import { pluginOperation } from '../plugin-operations'
import { formResult } from '../plugins'
import PluginFields from './PluginFields.vue'
import PluginResourceAuthorization from './PluginResourceAuthorization.vue'
import PluginResourceGrants from './PluginResourceGrants.vue'
import PluginResourceRoutes from './PluginResourceRoutes.vue'
import PluginView from './PluginView.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{
  plugin: string
  manifest: Manifest
  instance: string
  setupRevision?: string
  disabled?: boolean
}>()
const emit = defineEmits<{ state: [value: { busy: boolean, editing: boolean, pending: boolean }] }>()
const path = computed(() => `/plugins/${props.plugin}/${props.setupRevision ? 'setup-drafts' : 'connections'}/${props.instance}/resources`)
const pages = ref<Record<string, PluginPage<PluginResource>>>(Object.create(null))
const editing = ref<{ collection: PluginResourceCollection, resource?: PluginResource }>()
const name = ref('')
const parent = ref('')
const parentOptions = computed(() => {
  const collection = editing.value?.collection.parent
  return (collection ? pages.value[collection]?.items ?? [] : []).map(item => ({ value: item.id, label: item.name }))
})
const enabled = ref(true)
const values = ref<Record<string, any>>({})
const secrets = ref<Record<string, string>>(pluginFormSecrets())
const busy = ref(false)
const loading = ref(false)
const ready = ref(false)
const submitted = ref(false)
const completion = ref<FormResult>()
const completing = ref(false)
const formReady = ref(true)
const formFailure = ref('')
let asked = 0
let formTimer: ReturnType<typeof setTimeout> | undefined
const error = ref('')
const removing = ref<PluginResource>()
const accessEditors = ref<Record<string, boolean>>({})
const accessLocked = computed(() => Object.values(accessEditors.value).some(Boolean))
const authorizations = ref<Record<string, { busy: boolean, pending: boolean }>>({})
const oauthBusy = computed(() => Object.values(authorizations.value).some(value => value.busy))
const oauthPending = computed(() => Object.values(authorizations.value).some(value => value.pending))
const oauthLocked = computed(() => oauthBusy.value || oauthPending.value)
const blocked = computed(() => busy.value || loading.value || !ready.value || accessLocked.value || props.disabled || oauthLocked.value || submitted.value)
const saveBlocked = computed(() => blocked.value || completing.value || !formReady.value || Object.keys(completion.value?.errors ?? {}).length > 0)
const formMessages = computed(() => Object.entries(completion.value?.errors ?? {}).filter(([key]) => key === 'form' || key === 'parentId' || editing.value?.collection.settings?.some(field => field.key === key && field.hidden)).map(([, message]) => message))
let generation = 0

function accessBlocked(own: string) {
  return Object.entries(accessEditors.value).some(([key, active]) => key !== own && active)
}

function accessActivity(key: string, active: boolean) {
  const next = { ...accessEditors.value }
  if (active)
    next[key] = true
  else
    delete next[key]
  accessEditors.value = next
}

function authorizationKey(resource: PluginResource, provider: string) {
  return `${path.value}/${resource.collection}/${resource.id}/oauth/${provider}@${resource.revision}@${props.setupRevision ?? ''}`
}

function authorizationBlocked(resource: PluginResource, provider: string) {
  const own = authorizationKey(resource, provider)
  const prefix = `${path.value}/${resource.collection}/${resource.id}/oauth/`
  return Object.entries(authorizations.value).some(([key, activity]) => key !== own && key.startsWith(prefix) && (activity.busy || activity.pending))
}

function authorizationState(key: string, value: { busy: boolean, pending: boolean }) {
  const next = { ...authorizations.value }
  if (value.busy || value.pending)
    next[key] = value
  else
    delete next[key]
  authorizations.value = next
}

watch(() => [busy.value, loading.value, completing.value, Boolean(editing.value || removing.value), oauthBusy.value, oauthPending.value, accessLocked.value], () => {
  emit('state', { busy: busy.value || loading.value || oauthBusy.value || completing.value, editing: Boolean(editing.value || removing.value) || accessLocked.value, pending: oauthPending.value })
}, { immediate: true, flush: 'sync' })

async function load() {
  const current = ++generation
  loading.value = true
  ready.value = false
  error.value = ''
  try {
    const entries = await Promise.all((props.manifest.resources ?? []).map(async (collection) => {
      // A refresh must retain later-page widgets and their memory-only consents.
      const remaining = new Set((pages.value[collection.name]?.items ?? []).map(item => item.id))
      const items: PluginResource[] = []
      let page = await api<PluginPage<PluginResource>>(`${path.value}/${collection.name}`)
      for (;;) {
        items.push(...page.items)
        for (const item of page.items)
          remaining.delete(item.id)
        if (!remaining.size || !page.nextCursor)
          break
        page = await api<PluginPage<PluginResource>>(`${path.value}/${collection.name}?cursor=${encodeURIComponent(page.nextCursor)}`)
      }

      return [collection.name, { items, nextCursor: page.nextCursor }] as const
    }))
    if (current === generation) {
      pages.value = Object.assign(Object.create(null), Object.fromEntries(entries))
      ready.value = true
      return true
    }

    return false
  }
  catch (e) {
    if (current === generation)
      error.value = (e as Error).message
    return false
  }
  finally {
    if (current === generation)
      loading.value = false
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
    const current = generation
    const cursor = pages.value[collection]?.nextCursor
    if (!cursor)
      return
    const page = await api<PluginPage<PluginResource>>(`${path.value}/${collection}?cursor=${encodeURIComponent(cursor)}`)
    if (current === generation)
      pages.value[collection] = { items: [...pages.value[collection].items, ...page.items], nextCursor: page.nextCursor }
  })
}

async function edit(collection: PluginResourceCollection, resource?: PluginResource) {
  if (blocked.value)
    return
  await action(async () => {
    // Parent choices include later pages too; the native resource limit bounds this.
    if (collection.parent) {
      while (pages.value[collection.parent]?.nextCursor) {
        const page = pages.value[collection.parent]
        const next = await api<PluginPage<PluginResource>>(`${path.value}/${collection.parent}?cursor=${encodeURIComponent(page.nextCursor!)}`)
        pages.value[collection.parent] = { items: [...page.items, ...next.items], nextCursor: next.nextCursor }
      }
    }

    cancelCompletion()
    name.value = resource?.name ?? ''
    parent.value = resource?.parentId ?? ''
    enabled.value = resource?.enabled ?? true
    values.value = pluginFormValues(collection.settings ?? [], resource?.settings ?? {})
    secrets.value = pluginFormSecrets()
    editing.value = { collection, resource }
    formReady.value = !collection.form
    typed()
  })
}

function cancelCompletion() {
  asked++
  clearTimeout(formTimer)
  completing.value = false
  completion.value = undefined
  formFailure.value = ''
}

function formBody() {
  if (!editing.value)
    return {}
  const { collection, resource } = editing.value!
  return {
    settings: pluginSettings(collection.settings ?? [], values.value),
    secrets: pluginSecrets(secrets.value),
    ...(resource && { resource: resource.id, revision: resource.revision }),
    ...(collection.parent && parent.value && { parentId: parent.value }),
    ...(props.setupRevision && { setupRevision: props.setupRevision }),
  }
}

const lookup = pluginFormLookup(() => editing.value ? `${path.value}/${editing.value.collection.name}/form` : '', formBody, () => ({
  version: props.manifest.version,
  parentRevision: pages.value[editing.value?.collection.parent ?? '']?.items.find(item => item.id === parent.value)?.revision,
}))

async function complete(changed?: string) {
  const editor = editing.value
  if (!editor?.collection.form)
    return
  const request = ++asked
  formReady.value = false
  completing.value = true
  formFailure.value = ''
  try {
    const { collection } = editor
    const result = formResult(await pluginOperation<FormResult | null>(`${path.value}/${collection.name}/form`, {
      ...formBody(),
      changed,
    }))
    if (request !== asked)
      return
    for (const field of collection.settings ?? []) {
      if (Object.hasOwn(result.values, field.key))
        values.value[field.key] = pluginFormValue(field, result.values[field.key])
    }

    completion.value = result
    formReady.value = true
  }
  catch (e) {
    if (request === asked)
      formFailure.value = (e as Error).message
  }
  finally {
    if (request === asked)
      completing.value = false
  }
}

function typed(changed?: string) {
  if (!editing.value?.collection.form)
    return
  clearTimeout(formTimer)
  asked++
  formReady.value = false
  completing.value = true
  formTimer = setTimeout(() => void complete(changed), 350)
}

watch(parent, () => typed('parentId'))

function resetEditor() {
  cancelCompletion()
  editing.value = undefined
  secrets.value = pluginFormSecrets()
  submitted.value = false
}

async function reconcile() {
  if (busy.value || loading.value)
    return
  await action(async () => {
    if (await load())
      resetEditor()
  })
}

async function close() {
  if (busy.value)
    return
  if (submitted.value)
    await reconcile()
  else
    resetEditor()
}

async function save() {
  if (!editing.value || saveBlocked.value)
    return
  await action(async () => {
    const { collection, resource } = editing.value!
    submitted.value = true
    try {
      await pluginOperation(`${path.value}/${collection.name}${resource ? `/${resource.id}` : ''}`, {
        name: name.value,
        settings: pluginSettings(collection.settings ?? [], values.value),
        secrets: pluginSecrets(secrets.value),
        enabled: enabled.value,
        ...(collection.parent && { parentId: parent.value }),
        ...(resource && { revision: resource.revision }),
        ...(props.setupRevision && { setupRevision: props.setupRevision }),
      }, resource ? 'PUT' : 'POST')
    }
    catch (e) {
      if (e instanceof ApiError && e.status === 400)
        submitted.value = false
      throw e
    }

    resetEditor()
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

watch([path, () => props.setupRevision, () => props.manifest.version, () => JSON.stringify(props.manifest.resources)], () => {
  resetEditor()
  pages.value = Object.create(null)
  void load()
}, { immediate: true })
onBeforeUnmount(() => {
  cancelCompletion()
  generation++
  secrets.value = pluginFormSecrets()
  emit('state', { busy: false, editing: false, pending: false })
})
</script>

<template>
  <div class="grid min-w-0 gap-5" aria-label="Resources">
    <UiButton size="small" :disabled="busy || loading || (accessLocked && ready)" @click="load">
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
        <UiButton size="small" :disabled="blocked" @click="edit(collection)">
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
            :disabled="blocked"
            :aria-label="`Edit ${resource.name}`"
            @click="edit(collection, resource)"
          >
            Edit
          </UiButton>
          <UiButton
            size="small"
            :disabled="blocked"
            :aria-label="`Delete ${resource.name}`"
            @click="removing = resource"
          >
            Delete
          </UiButton>
        </div>
        <PluginResourceGrants
          v-if="!setupRevision && manifest.agentAccess && manifest.operations?.some(operation => operation.collection === collection.name)"
          :key="`grants/${resource.id}/${resource.revision}`"
          :path="`${path}/${collection.name}/${resource.id}/grants`"
          :name="resource.name"
          :operations="manifest.operations!.filter(operation => operation.collection === collection.name)"
          :enabled="ready && !submitted && !disabled && !busy && !loading && !editing && !removing && !oauthLocked && !accessBlocked(`grants/${resource.id}`)"
          @activity="accessActivity(`grants/${resource.id}`, $event)"
        />
        <PluginResourceRoutes
          v-if="!setupRevision && collection.delegation"
          :key="`routes/${resource.id}/${resource.revision}`"
          :path="`${path}/${collection.name}/${resource.id}`"
          :name="resource.name"
          :enabled="ready && !submitted && !disabled && !busy && !loading && !editing && !removing && !oauthLocked && !accessBlocked(`routes/${resource.id}`)"
          @activity="accessActivity(`routes/${resource.id}`, $event)"
        />
        <PluginResourceAuthorization
          v-for="provider in (manifest.oauth ?? []).filter(provider => provider.collection === collection.name && !provider.personal)"
          :key="authorizationKey(resource, provider.name)"
          :path="`${path}/${collection.name}/${resource.id}/oauth/${provider.name}`"
          :provider="provider"
          :enabled="ready && !submitted && !disabled && !busy && !loading && !editing && !accessLocked"
          :blocked="authorizationBlocked(resource, provider.name)"
          :setup-revision="setupRevision"
          @state="authorizationState(authorizationKey(resource, provider.name), $event)"
          @changed="load"
        />
        <div v-if="removing?.id === resource.id" class="grid gap-2 rounded-lg bg-inset p-3" role="alert">
          <p class="text-xs">
            Delete {{ resource.name }} and its child resources? Their saved credentials and access rules will be removed.
          </p>
          <div class="flex gap-2">
            <UiButton size="small" :disabled="busy" @click="removing = undefined">
              Cancel
            </UiButton>
            <UiButton size="small" :disabled="blocked" @click="remove">
              Confirm delete
            </UiButton>
          </div>
        </div>
      </article>
      <UiButton
        v-if="pages[collection.name]?.nextCursor"
        size="small"
        :disabled="busy || loading || accessLocked || !ready || submitted"
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
        <UiAlert v-if="submitted && !busy">
          Check whether this resource was saved before making another change.
          <UiButton
            type="button"
            size="small"
            :disabled="busy || loading"
            @click="reconcile"
          >
            Check save result
          </UiButton>
        </UiAlert>
        <label>Name<input
          v-model="name"
          :disabled="blocked"
          required
          maxlength="100"
        ></label>
        <VirtualSelect
          v-if="collection.parent"
          v-model="parent"
          label="Parent"
          placeholder="Choose a parent"
          :options="parentOptions"
          :disabled="blocked"
          required
        />
        <div @input="typed()">
          <PluginFields
            v-model:values="values"
            v-model:secrets="secrets"
            :fields="(collection.settings ?? []).map(field => setupRevision ? { ...field, required: false } : field)"
            :lookup="lookup"
            :saved="editing.resource?.secretKeys"
            :disabled="blocked"
            :options="completion?.options"
            :errors="completion?.errors"
            @refresh="typed"
          />
        </div>
        <PluginView v-if="completion?.view?.length" :nodes="completion.view" busy />
        <UiAlert v-for="(message, index) in formMessages" :key="index">
          {{ message }}
        </UiAlert>
        <UiAlert v-if="formFailure">
          {{ formFailure }}
          <UiButton
            type="button"
            size="small"
            :disabled="blocked || completing"
            @click="complete()"
          >
            Retry checking
          </UiButton>
        </UiAlert>
        <p v-if="completing" class="text-xs text-muted" role="status">
          Checking resource settings…
        </p>
        <label class="flex flex-row! items-center gap-2!"><input
          v-model="enabled"
          type="checkbox"
          class="w-auto!"
          :disabled="blocked"
        >Enabled</label>
        <div class="flex gap-2">
          <UiButton type="button" :disabled="busy" @click="close">
            Cancel
          </UiButton>
          <UiButton type="submit" variant="primary" :disabled="saveBlocked">
            Save {{ collection.singular.toLowerCase() }}
          </UiButton>
        </div>
      </form>
    </section>
  </div>
</template>
