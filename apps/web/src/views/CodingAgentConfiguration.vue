<script setup lang="ts">
import type { Provider } from '../../../../packages/contracts/accounts'
import type { CodingAgentConfiguration } from '../../../../packages/contracts/generated/typescript/CodingAgentConfiguration'
import type { CodingAgentSetting } from '../../../../packages/contracts/generated/typescript/CodingAgentSetting'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { onBeforeRouteLeave, onBeforeRouteUpdate, useRoute } from 'vue-router'
import { providers } from '../../../../packages/contracts/accounts'
import { getCodingAgentConfiguration } from '../../../../packages/contracts/generated/getCodingAgentConfiguration'
import { previewCodingAgentConfiguration } from '../../../../packages/contracts/generated/previewCodingAgentConfiguration'
import { saveCodingAgentConfiguration } from '../../../../packages/contracts/generated/saveCodingAgentConfiguration'
import {
  api,
  notify,
  refresh,
  state,
} from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import ProviderMark from '../components/ProviderMark.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import UiSegments from '../components/UiSegments.vue'
import {
  ArrowLeft,
  Check,
  Code,
  RotateCcw,
} from '../icons'

const route = useRoute()
const provider = computed(() => route.params.provider as Provider)
const agentId = computed(() => typeof route.params.id === 'string' ? route.params.id : null)
const current = ref<CodingAgentConfiguration>()
const document = ref('')
const saved = ref('')
const mode = ref<'form' | 'text'>('form')
const loading = ref(false)
const busy = ref(false)
const checking = ref(false)
const error = ref('')
const validation = ref('')
const fieldErrors = ref<Record<string, string>>({})
const fieldError = computed(() => Object.values(fieldErrors.value).join(' '))
const fieldDrafts = ref<Record<string, string>>({})
const pending = new Map<string, string>()
const inheritedResets = ref(new Set<string>())
const conflict = ref(false)
const search = ref('')
const leaving = ref(false)
let leave: ((value: boolean) => void) | undefined
let generation = 0
let timer: ReturnType<typeof setTimeout> | undefined
let replacing = false
const dirty = computed(() => document.value !== saved.value || !!fieldError.value || Object.keys(fieldDrafts.value).length > 0 || inheritedResets.value.size > 0)
const back = computed(() => agentId.value ? `/agents/${agentId.value}?section=model` : '/settings/installation')
const backLabel = computed(() => agentId.value ? 'Back to agent' : 'Back to installation settings')
const groups = computed(() => {
  const groups = new Map<string, CodingAgentSetting[]>()
  for (const field of current.value?.settings ?? []) {
    if (search.value && !`${field.label} ${field.path} ${field.description}`.toLowerCase().includes(search.value.toLowerCase()))
      continue
    const rows = groups.get(field.group) ?? []
    rows.push(field)
    groups.set(field.group, rows)
  }

  return [...groups]
})
const customCount = computed(() => current.value?.customizedPaths.length ?? 0)

function input(changes: { path: string, value: string }[] = [], reset = false) {
  return {
    provider: provider.value,
    agentId: agentId.value,
    revision: current.value!.revision,
    document: document.value,
    changes,
    inheritPaths: [...inheritedResets.value],
    reset,
  }
}

async function load() {
  if (state.installationRole !== 'owner')
    return
  const request = ++generation
  clearTimeout(timer)
  loading.value = true
  error.value = ''
  try {
    const result = await getCodingAgentConfiguration(api, { provider: provider.value, agentId: agentId.value })
    if (request !== generation)
      return
    current.value = result
    replacing = true
    document.value = result.document
    saved.value = result.document
    replacing = false
    validation.value = ''
    fieldErrors.value = {}
    fieldDrafts.value = {}
    pending.clear()
    inheritedResets.value.clear()
    conflict.value = false
  }
  catch (e) {
    if (request === generation)
      error.value = (e as Error).message
  }
  finally {
    if (request === generation)
      loading.value = false
  }
}

async function preview(changes: { path: string, value: string }[] = [], reset = false) {
  if (!current.value || busy.value)
    return
  clearTimeout(timer)
  for (const change of changes) {
    pending.set(change.path, change.value)
    if (change.value === 'null')
      inheritedResets.value.add(change.path)
    else
      inheritedResets.value.delete(change.path)
  }

  const request = ++generation
  checking.value = true
  const fromForm = pending.size > 0
  const submitted = new Map(pending)
  try {
    const result = await previewCodingAgentConfiguration(api, input([...pending].map(([path, value]) => ({ path, value })), reset))
    if (request !== generation)
      return
    current.value = result
    replacing = true
    document.value = result.document
    replacing = false
    validation.value = ''
    for (const path of submitted.keys()) {
      delete fieldDrafts.value[path]
      pending.delete(path)
    }

    delete fieldErrors.value.$native
    for (const path of result.customizedPaths)
      inheritedResets.value.delete(path)
  }
  catch (e) {
    if (request === generation) {
      if (fromForm)
        fieldErrors.value.$native = (e as Error).message
      else
        validation.value = (e as Error).message
    }
  }
  finally {
    if (request === generation)
      checking.value = false
  }
}

watch(document, () => {
  if (replacing || loading.value)
    return
  fieldDrafts.value = {}
  fieldErrors.value = {}
  pending.clear()
  clearTimeout(timer)
  checking.value = true
  ++generation
  timer = setTimeout(() => void preview(), 350)
}, { flush: 'sync' })

function value(field: CodingAgentSetting) {
  if (Object.hasOwn(fieldDrafts.value, field.path))
    return fieldDrafts.value[field.path]
  return field.value === null ? '' : JSON.parse(field.value)
}

function change(field: CodingAgentSetting, event: Event) {
  const entered = (event.target as HTMLInputElement).value
  fieldDrafts.value[field.path] = entered
  inheritedResets.value.delete(field.path)
  clearTimeout(timer)
  ++generation
  let chosen: unknown = entered === '' ? null : entered
  if (entered !== '' && field.kind === 'integer') {
    const number = Number(entered)
    if (!Number.isSafeInteger(number) || number <= 0) {
      fieldErrors.value[field.path] = `${field.label}: enter a positive whole number.`
      pending.delete(field.path)
      checking.value = false
      return
    }

    chosen = number
  }

  if (entered !== '' && field.kind === 'boolean')
    chosen = entered === 'true'
  delete fieldErrors.value[field.path]
  delete fieldErrors.value.$native
  pending.set(field.path, JSON.stringify(chosen))
  checking.value = true
  timer = setTimeout(() => void preview(), 350)
}

async function save() {
  if (!dirty.value || checking.value || validation.value || fieldError.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    const result = await saveCodingAgentConfiguration(api, input())
    current.value = result
    replacing = true
    document.value = result.document
    saved.value = result.document
    replacing = false
    conflict.value = false
    inheritedResets.value.clear()
    notify('Coding agent configuration saved')
    await refresh()
  }
  catch (e) {
    error.value = (e as Error).message
    conflict.value = error.value.includes('changed')
  }
  finally {
    busy.value = false
  }
}

function resetAll() {
  for (const path of current.value?.customizedPaths ?? [])
    inheritedResets.value.add(path)
  void preview([], true)
}

function shortcut(event: KeyboardEvent) {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 's') {
    event.preventDefault()
    void save()
  }
}

function beforeUnload(event: BeforeUnloadEvent) {
  if (dirty.value) {
    event.preventDefault()
    event.returnValue = ''
  }
}

function answerLeave(value: boolean) {
  leaving.value = false
  leave?.(value)
  leave = undefined
}

function confirmNavigation() {
  if (!dirty.value)
    return true
  leaving.value = true
  return new Promise<boolean>((resolve) => {
    leave = resolve
  })
}

onBeforeRouteLeave(confirmNavigation)
onBeforeRouteUpdate(confirmNavigation)
watch(() => [provider.value, agentId.value], load, { immediate: true })
onMounted(() => {
  window.addEventListener('keydown', shortcut)
  window.addEventListener('beforeunload', beforeUnload)
})
onBeforeUnmount(() => {
  ++generation
  clearTimeout(timer)
  window.removeEventListener('keydown', shortcut)
  window.removeEventListener('beforeunload', beforeUnload)
})
</script>

<template>
  <div class="mx-auto w-full max-w-5xl pb-24">
    <RouterLink :to="back" class="mb-7 inline-flex min-h-11 items-center gap-2 text-sm text-muted hover:text-ink">
      <Icon :name="ArrowLeft" :size="16" />{{ backLabel }}
    </RouterLink>
    <header class="mb-8 flex items-start gap-4">
      <ProviderMark :provider="provider" :size="52" />
      <div class="min-w-0">
        <p class="mb-1 text-2xs font-semibold uppercase tracking-widest text-muted">
          {{ agentId ? current?.agentName || 'Agent configuration' : 'Installation defaults' }}
        </p>
        <h1 class="text-2xl font-heading">
          {{ providers[provider]?.label }} configuration
        </h1>
        <p class="mt-2 max-w-2xl text-sm leading-relaxed text-muted">
          {{ agentId ? 'Personalize this agent. Other values continue to follow the installation defaults.' : 'Choose the defaults agents inherit. Each agent can personalize its own settings.' }}
        </p>
      </div>
    </header>
    <UiAlert v-if="state.installationRole !== 'owner'">
      Only the installation owner can edit coding agent configuration.
    </UiAlert>
    <template v-else>
      <UiAlert v-if="error" class="mb-5">
        {{ error }}
        <UiButton
          v-if="!current || conflict"
          size="small"
          class="mt-3"
          @click="load"
        >
          {{ conflict ? 'Reload latest configuration' : 'Try again' }}
        </UiButton>
      </UiAlert>
      <p v-if="loading" role="status" class="py-12 text-sm text-muted">
        Loading configuration…
      </p>
      <template v-else-if="current">
        <div class="mb-5 flex flex-wrap items-center justify-between gap-3 border-b border-line pb-5">
          <UiSegments
            v-model="mode"
            label="Configuration editor"
            :options="[{ value: 'form', label: 'Settings' }, { value: 'text', label: current.filename, icon: Code }]"
            compact
            :disabled="checking"
          />
          <span class="text-xs text-muted">{{ agentId ? `${customCount} personalized ${customCount === 1 ? 'setting' : 'settings'}` : 'Shared by this installation' }}</span>
          <UiButton size="small" :disabled="busy || checking || !customCount || customCount > 1000" @click="resetAll">
            {{ agentId ? 'Reset all customizations' : 'Restore engine defaults' }}
          </UiButton>
        </div>
        <div v-if="validation || fieldError" class="mb-5">
          <UiAlert>{{ fieldError || validation }} <span v-if="mode === 'form' && validation">Open the text editor to correct the document.</span></UiAlert>
        </div>
        <template v-if="mode === 'form'">
          <label class="mb-6 block"><span class="sr-only">Find a setting</span><input
            v-model="search"
            type="search"
            placeholder="Find a setting…"
            aria-label="Find a setting"
          ></label>
          <div v-if="!groups.length" class="rounded-xl border border-line p-8 text-center text-sm text-muted">
            No matching settings. The text editor also supports advanced native options.
          </div>
          <section v-for="[group, fields] in groups" :key="group" class="mb-7 overflow-hidden rounded-2xl border border-line bg-surface">
            <h2 class="border-b border-line bg-soft/50 px-6 py-4 text-sm font-semibold">
              {{ group }}
            </h2>
            <div v-for="field in fields" :key="field.path" class="grid grid-cols-[minmax(0,1fr)_minmax(180px,260px)] gap-6 border-b border-line px-6 py-5 last:border-0 phone:grid-cols-1 phone:gap-3 phone:px-4">
              <div>
                <label :for="`native-${field.path}`" class="text-sm font-semibold">{{ field.label }}</label>
                <p class="mt-1.5 text-xs leading-relaxed text-muted">
                  {{ field.description }}
                </p>
                <span v-if="agentId" class="mt-2 inline-block text-micro" :class="field.inherited ? 'text-muted' : 'text-accent'">{{ field.inherited ? 'Inherited' : 'Personalized for this agent' }}</span>
              </div>
              <div class="min-w-0 self-center">
                <select
                  v-if="field.kind === 'select' || field.kind === 'boolean'"
                  :id="`native-${field.path}`"
                  :value="field.value === null ? '' : String(value(field))"
                  :disabled="busy || !!validation"
                  @change="change(field, $event)"
                >
                  <option value="">
                    {{ agentId ? 'Follow installation' : 'Automatic' }}
                  </option>
                  <template v-if="field.kind === 'boolean'">
                    <option value="true">
                      Enabled
                    </option><option value="false">
                      Disabled
                    </option>
                  </template>
                  <option v-for="choice in field.choices" :key="choice" :value="choice">
                    {{ choice }}
                  </option>
                </select>
                <textarea
                  v-else-if="field.path === '/developer_instructions'"
                  :id="`native-${field.path}`"
                  :value="value(field)"
                  :disabled="busy || !!validation"
                  rows="4"
                  @input="change(field, $event)"
                />
                <input
                  v-else
                  :id="`native-${field.path}`"
                  :type="field.kind === 'integer' ? 'number' : 'text'"
                  :min="field.kind === 'integer' ? 1 : undefined"
                  :step="field.kind === 'integer' ? 1 : undefined"
                  :value="value(field)"
                  :placeholder="agentId && field.inherited ? 'Follow installation' : 'Engine default'"
                  :disabled="busy || !!validation"
                  @input="change(field, $event)"
                >
                <button
                  v-if="agentId ? !field.inherited : field.value !== null"
                  type="button"
                  class="mt-1.5 inline-flex min-h-8 items-center gap-1.5 text-2xs text-muted hover:text-accent"
                  :disabled="busy || checking"
                  @click="preview([{ path: field.path, value: 'null' }])"
                >
                  <Icon :name="RotateCcw" :size="12" />{{ agentId ? 'Use installation value' : 'Use engine default' }}
                </button>
              </div>
            </div>
          </section>
          <button
            type="button"
            class="inline-flex min-h-11 items-center gap-2 text-sm font-medium text-accent disabled:opacity-50"
            :disabled="checking || busy"
            @click="mode = 'text'"
          >
            <Icon :name="Code" :size="17" />Edit advanced native options
          </button>
        </template>
        <div v-else class="overflow-hidden rounded-xl border border-line bg-surface">
          <div class="flex flex-wrap items-center justify-between gap-2 border-b border-line bg-soft px-4 py-3">
            <span class="font-mono text-xs">{{ current.filename }}</span><span class="text-2xs text-muted">{{ agentId ? 'Effective configuration · including inherited values' : 'Native configuration' }}</span>
          </div>
          <textarea
            v-model="document"
            aria-label="Native configuration"
            class="block min-h-[420px] w-full resize-y rounded-none border-0 bg-transparent p-5 font-mono text-xs leading-6 outline-none phone:min-h-[360px]"
            spellcheck="false"
            autocapitalize="off"
            autocomplete="off"
            autocorrect="off"
            wrap="off"
            :disabled="busy"
          />
          <div class="border-t border-line px-4 py-3 text-2xs text-muted" role="status">
            {{ checking ? 'Checking configuration…' : validation ? 'Correct the document before saving.' : 'Document checked. Advanced native options are preserved when switching editors.' }}
          </div>
        </div>
        <details class="mt-8 rounded-xl border border-line px-5 py-4 text-xs">
          <summary class="cursor-pointer font-semibold">
            Managed by Cairn
          </summary>
          <p class="mt-3 leading-relaxed text-muted">
            These settings remain controlled by Cairn. Credentials and temporary run tokens are never shown in this editor.
          </p>
          <dl class="mt-4 grid gap-3">
            <div v-for="item in current.managed" :key="item.path">
              <dt class="font-mono text-2xs">
                {{ item.path }}
              </dt><dd class="mt-1 leading-relaxed text-muted">
                {{ item.reason }}
              </dd>
            </div>
          </dl>
        </details>
        <div class="mt-6 grid gap-1.5 text-xs leading-relaxed text-muted">
          <p v-for="notice in current.notices" :key="notice">
            {{ notice }}
          </p>
        </div>
        <footer class="sticky bottom-0 z-10 mt-8 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-line bg-surface/95 p-4 shadow-lg backdrop-blur phone:pb-[max(16px,env(safe-area-inset-bottom))]">
          <span class="inline-flex items-center gap-2 text-xs" :class="dirty ? 'text-accent' : 'text-muted'"><Icon v-if="!dirty" :name="Check" :size="15" />{{ dirty ? 'Unsaved changes' : 'All changes saved' }}</span>
          <div class="flex items-center gap-2">
            <UiButton :disabled="busy || checking || !dirty" @click="load">
              Discard
            </UiButton><UiButton variant="primary" :disabled="!dirty || busy || checking || !!validation || !!fieldError" @click="save">
              {{ busy ? 'Saving…' : 'Save configuration' }}
            </UiButton>
          </div>
        </footer>
      </template>
    </template>
    <Modal v-if="leaving" title="Discard configuration changes?" @close="answerLeave(false)">
      <p class="mb-5 text-sm text-muted">
        Your changes have not been saved.
      </p><div class="flex justify-end gap-2">
        <UiButton @click="answerLeave(false)">
          Keep editing
        </UiButton><UiButton variant="primary" @click="answerLeave(true)">
          Discard changes
        </UiButton>
      </div>
    </Modal>
  </div>
</template>
