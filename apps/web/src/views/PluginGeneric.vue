<script setup lang="ts">
import type { Agent } from '../../../../packages/contracts/contracts'
import type { SetupStep } from '../plugin-setup'
import type {
  Catalog,
  FormResult,
  PluginConnection,
  PluginEntry,
  SettingField,
} from '../plugins'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from 'vue'
import { useRoute } from 'vue-router'
import {
  api,
  notify,
  relative,
  state,
} from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import PersonAvatar from '../components/PersonAvatar.vue'
import PluginChecklist from '../components/PluginChecklist.vue'
import PluginFields from '../components/PluginFields.vue'
import PluginInstancePanel from '../components/PluginInstancePanel.vue'
import PluginMark from '../components/PluginMark.vue'
import PluginPresets from '../components/PluginPresets.vue'
import PluginPrograms from '../components/PluginPrograms.vue'
import PluginReview from '../components/PluginReview.vue'
import PluginView from '../components/PluginView.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import {
  credentialInput,
  signInProblem,
  useDeviceSignIn,
  verificationLink,
} from '../device-sign-in'
import {
  ArrowUpRight,
  Check,
  ChevronRight,
  Copy,
  KeyRound,
  Pencil,
  Plus,
  ShieldCheck,
  Terminal,
  Trash2,
  TriangleAlert,
  Wrench,
  X,
  Zap,
} from '../icons'
import {
  pluginFieldAnswer,
  pluginFormSecrets,
  pluginFormValue,
  pluginFormValues,
  pluginSecrets,
  pluginSettings,
} from '../plugin-form-values'
import { pluginOperation } from '../plugin-operations'
import {
  effortLabels,
  matchingPreset,
  pluginSetup,
  sharedSources,
  shortValue,
  stepFields,
} from '../plugin-setup'
import {
  capabilityLabels,
  commandUsage,
  connectionState,
  contributions,
  delegates as delegating,
  delegationRisks,
  instanceNoun,
  listed,
  personName,
} from '../plugins'
import { buttonBase, buttonSizes, buttonVariants } from '../ui'

const route = useRoute()
const id = computed(() => String(route.params.id))
const catalog = ref<Catalog>({ packages: [], capabilities: [], connections: [] })
const loaded = ref(false)
const busy = ref(false)
const error = ref('')
const reviewing = ref(false)
const editing = ref<PluginConnection | null>()
const removing = ref<PluginConnection>()
const form = ref<Record<string, any>>({})
const values = ref<Record<string, any>>({})
const secrets = ref<Record<string, string>>(pluginFormSecrets())
const people = ref<{ id: string, name: string }[]>([])
const person = ref('')
const completion = ref<FormResult | null>(null)
const completionInputs = ref<Record<string, string>>({})
const completing = ref(false)
const copied = ref(false)
// The setup assistant: its current step, the saved instance whose shared setup (such as
// an OAuth client) a new one reuses, and the agents granted when it is created.
const step = ref(0)
const reuse = ref('')
const grants = ref<string[]>([])
const named = ref(false)
let timer: ReturnType<typeof setInterval> | undefined
let typing: ReturnType<typeof setTimeout> | undefined
let asked = 0

const entry = computed<PluginEntry | undefined>(() => catalog.value.packages.find(item => item.id === id.value))
const manifest = computed(() => entry.value?.manifest)
const connections = computed(() => catalog.value.connections.filter(item => item.pluginId === id.value))
const delegates = computed(() => delegating(manifest.value))
const fields = computed(() => (manifest.value?.settings ?? []).filter(field => !field.hidden))
const plainFields = computed(() => fields.value.filter(field => field.type !== 'secret'))
const secretFields = computed(() => fields.value.filter(field => field.type === 'secret'))

// A package error describes the input it checked. Ordinary fields need not request a
// refresh while typing; editing them removes stale errors and save still validates them.
function fieldInput(field: SettingField) {
  return JSON.stringify(field.type === 'secret' ? secrets.value[field.key] : values.value[field.key]) ?? ''
}

const formErrors = computed(() => Object.fromEntries(Object.entries(completion.value?.errors ?? {}).filter(([key]) => {
  const field = fields.value.find(field => field.key === key)
  return !field || pluginFieldAnswer(completionInputs.value, key) === fieldInput(field)
})))
// Packages with their own panel describe the instance there.
const shownFields = computed(() => manifest.value?.instanceView ? [] : plainFields.value)
// Hidden settings are the package's own (e.g. names it derives); editing keeps them.
const storedFields = computed(() => (manifest.value?.settings ?? []).filter(field => field.type !== 'secret'))
const setup = computed(() => entry.value ? pluginSetup(entry.value) : { steps: [] })
// A package with nothing to enter still names its instance and turns it on.
const steps = computed<SetupStep[]>(() => setup.value.steps.length ? setup.value.steps : [{ id: 'details', kind: 'settings', title: 'Details' }])
const current = computed<SetupStep | undefined>(() => steps.value[Math.min(step.value, steps.value.length - 1)])
const last = computed(() => step.value >= steps.value.length - 1)
const sources = computed(() => sharedSources(steps.value, manifest.value?.settings ?? [], connections.value, editing.value?.id ?? undefined))
// The installation copies the reused values, secrets included, without sending them here.
const sharing = computed(() => reuse.value ? { sharedFrom: reuse.value } : {})
const sharedKeys = computed(() => steps.value.filter(item => item.shared).flatMap(item => item.settings ?? []))
// A package whose runs receive the instance's credentials gives each agent one instance.
const onePerAgent = computed(() => Boolean(manifest.value?.runCredentials && Object.keys(manifest.value.runCredentials).some(key => key === 'files' || key === 'git')))
const callbackShown = computed(() => steps.value.some(item => item.checklist?.some(task => task.copy === 'callbackUrl')))
const activityTones = {
  working: ['bg-soft text-accent', 'Working'],
  replied: ['bg-[light-dark(#dff3ee,#163a33)] text-success', 'Replied'],
  failed: ['bg-coral-soft text-coral', 'Not delivered'],
  stopped: ['bg-hover text-muted', 'Stopped'],
} as const

/** What this instance's people could do through an agent, for the owner's warning. */
function risks(agent: Agent) {
  return delegationRisks(agent, catalog.value.connections, form.value.accessMode === 'everyone', editing.value ? [] : grants.value)
}

const agents = computed(() => [...state.agents].sort((a, b) => Number(risks(a).length > 0) - Number(risks(b).length > 0)))
const chosenAgent = computed(() => state.agents.find(agent => agent.id === form.value.agentId))
const chosenRisks = computed(() => chosenAgent.value ? risks(chosenAgent.value) : [])
const singular = computed(() => instanceNoun(manifest.value, 1))
const plural = computed(() => instanceNoun(manifest.value, 2))
// A completed device sign-in gives the draft its credential; the package's form handler then
// says who it signed in as.
const oauthCallbackUrl = ref('')
const signIn = useDeviceSignIn(() => id.value, () => void complete('credential'), 2000, {
  kind: () => manifest.value?.credential?.oauth ? 'oauth' : 'device-flow',
  input: () => ({
    ...(editing.value ? { instance: editing.value.id } : {}),
    settings: pluginSettings(storedFields.value, values.value),
    secrets: pluginSecrets(secrets.value),
    ...sharing.value,
  }),
})
// A sign-in holds the OAuth client and scopes it asked for: choosing another app or other
// permissions afterwards needs a new sign-in rather than a save the installation refuses.
const signInBasis = computed(() => {
  const oauth = manifest.value?.credential?.oauth
  if (!oauth)
    return ''
  const keys = [oauth.clientIdSetting, ...Object.keys(oauth.scopeSettings ?? {})]
  const secret = oauth.clientSecretSetting ? secrets.value[oauth.clientSecretSetting] ?? '' : ''
  return JSON.stringify([reuse.value, keys.map(key => values.value[key] ?? null), secret])
})
watch(signInBasis, () => {
  if (!signIn.flow.value)
    return
  void signIn.cancel()
  void complete('credential')
})
const tones = {
  ok: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  muted: 'bg-hover text-muted',
  coral: 'bg-coral-soft text-coral',
}

async function load() {
  catalog.value = await api<Catalog>('/plugins')
  loaded.value = true
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

function display(value: unknown) {
  if (Array.isArray(value))
    return value.length ? value.join(', ') : '—'
  if (typeof value === 'boolean')
    return value ? 'On' : 'Off'
  return value === undefined || value === null || value === '' ? '—' : String(value)
}

function agentNames(connection: PluginConnection) {
  return (connection.agentIds ?? []).map(agent => state.agents.find(item => item.id === agent)?.name ?? 'Removed agent')
}

function edit(connection: PluginConnection | null) {
  error.value = ''
  void signIn.cancel()
  copied.value = false
  editing.value = connection
  values.value = pluginFormValues(storedFields.value, connection?.settings)
  secrets.value = pluginFormSecrets()
  people.value = (connection?.userIds ?? []).map(user => ({ id: user, name: connection?.people.find(item => item.id === user)?.name ?? '' }))
  person.value = ''
  completion.value = null
  step.value = 0
  grants.value = []
  named.value = Boolean(connection)
  reuse.value = connection ? '' : sharedSources(steps.value, manifest.value?.settings ?? [], connections.value)[0]?.id ?? ''
  if (!connection)
    reused(reuse.value)
  form.value = {
    name: connection?.name ?? (manifest.value?.instances ? '' : manifest.value?.name ?? ''),
    token: '',
    agentId: connection?.agentId ?? '',
    accessMode: connection?.accessMode ?? 'requests',
    maxMessagesPerHour: connection?.maxMessagesPerHour ?? 20,
    maxPerPersonPerHour: connection?.maxPerPersonPerHour ?? 5,
    enabled: connection?.enabled ?? true,
  }
  oauthCallbackUrl.value = ''
  if (manifest.value?.credential?.oauth) {
    void api<{ callbackUrl: string }>(`/plugins/${id.value}/oauth`)
      .then(result => oauthCallbackUrl.value = result.callbackUrl)
      .catch(e => error.value = (e as Error).message)
  }

  void complete()
}

/**
 * Asks the package's form handler for choices, derived values, errors and guidance.
 * Only the latest answer counts.
 */
async function complete(changed?: string) {
  if (!manifest.value?.instanceForm)
    return
  const ask = ++asked
  const inputs = Object.fromEntries(fields.value.map(field => [field.key, fieldInput(field)]))
  completing.value = true
  try {
    const result = await pluginOperation<FormResult>(`/plugins/${id.value}/form`, {
      instance: editing.value?.id,
      settings: pluginSettings(storedFields.value, values.value),
      secrets: pluginSecrets(secrets.value),
      ...sharing.value,
      ...credentialInput(form.value.token, signIn.flow.value),
      ...(changed ? { changed } : {}),
    })
    if (ask !== asked)
      return
    for (const field of storedFields.value) {
      if (Object.hasOwn(result.values, field.key)) {
        values.value[field.key] = pluginFormValue(field, result.values[field.key])
        inputs[field.key] = fieldInput(field)
      }
    }

    // A new instance is named after what the package verified, e.g. the mailbox.
    const nameFrom = setup.value.nameFrom
    if (nameFrom && !named.value && values.value[nameFrom])
      form.value.name = String(values.value[nameFrom])
    completionInputs.value = inputs
    completion.value = result
  }
  catch (e) {
    if (ask === asked) {
      completion.value = {
        options: completion.value?.options ?? {},
        values: {},
        errors: { form: (e as Error).message },
        view: [],
      }
    }
  }
  finally {
    if (ask === asked)
      completing.value = false
  }
}

async function presetChanged(keys: string[]) {
  for (const key of keys)
    await complete(key)
}

function typed() {
  // A pasted token replaces a sign-in.
  if (signIn.flow.value)
    void signIn.cancel()
  if (!manifest.value?.instanceForm)
    return
  clearTimeout(typing)
  // Saving waits for the package's answer about the new credential.
  completing.value = true
  typing = setTimeout(() => void complete('credential'), 500)
}

/** Shows the reused instance's visible shared values; another app starts empty. */
function reused(source: string) {
  const from = connections.value.find(item => item.id === source)
  for (const field of storedFields.value.filter(field => sharedKeys.value.includes(field.key)))
    values.value[field.key] = pluginFormValue(field, from?.settings[field.key] ?? '')
  for (const key of sharedKeys.value)
    delete secrets.value[key]
}

function chooseSource(source: string) {
  reuse.value = source
  reused(source)
  void complete()
}

function filled(field: SettingField) {
  if (field.type === 'secret')
    return Boolean(pluginFieldAnswer(pluginSecrets(secrets.value), field.key)) || Boolean(editing.value?.secretKeys?.includes(field.key))
  const value = pluginFieldAnswer(pluginSettings([field], values.value), field.key)
  if (field.type === 'boolean')
    return true
  if (field.type === 'multiselect')
    return Array.isArray(value) && value.length > 0
  return value !== undefined && value !== null && value !== ''
}

/** What a step still needs before the owner can go on, or '' when it is complete. */
function stepProblem(item: SetupStep | undefined): string {
  if (!item || !manifest.value)
    return ''
  if (item.kind === 'settings') {
    if (item.shared && reuse.value)
      return ''
    const shown = stepFields(item, fields.value)
    const missing = shown.find(field => field.required && !filled(field))
    if (missing)
      return `Enter ${missing.title}.`
    return shown.map(field => pluginFieldAnswer(formErrors.value, field.key)).find(Boolean) ?? ''
  }

  if (item.kind === 'credential') {
    const credential = manifest.value.credential
    const refused = pluginFieldAnswer(formErrors.value, 'credential')
    if (refused)
      return refused
    if (editing.value || signIn.completed.value)
      return ''
    if (credential?.oauth)
      return 'Sign in to continue.'
    return form.value.token ? '' : `Enter the ${credential?.label.toLowerCase() ?? 'credential'}.`
  }

  if (item.kind === 'people') {
    if (!chosenAgent.value)
      return 'Choose the agent that answers.'
    if (form.value.accessMode === 'allowlist' && !people.value.length)
      return 'Add at least one person.'
  }

  return ''
}

/** Steps before the first incomplete one can be opened; an edit opens any. */
function reachable(index: number) {
  return Boolean(editing.value) || steps.value.slice(0, index).every(item => !stepProblem(item))
}

function next() {
  if (editing.value || last.value)
    void save()
  else if (!stepProblem(current.value))
    step.value++
}

/** Fields the current step shows besides its presets' switches. */
function stepShown(item: SetupStep) {
  const shown = stepFields(item, fields.value)
  return item.presets?.length ? shown.filter(field => field.type !== 'boolean') : shown
}

function switches(item: SetupStep) {
  return stepFields(item, fields.value).filter(field => field.type === 'boolean')
}

/** Package guidance belongs with the credential and with choices the package fills. */
function guided(item: SetupStep) {
  return item.kind === 'credential' || (!manifest.value?.credential && item === steps.value[0])
    || stepFields(item, fields.value).some(field => field.dynamic || field.refresh)
}

function holder(agent: Agent) {
  return onePerAgent.value ? connections.value.find(item => item.agentIds?.includes(agent.id))?.name ?? '' : ''
}

function toggleGrant(agent: string, checked: boolean) {
  grants.value = checked ? [...grants.value, agent] : grants.value.filter(item => item !== agent)
}

function addPerson() {
  const value = person.value.trim()
  if (!value || people.value.some(item => item.id === value))
    return
  people.value.push({ id: value, name: catalog.value.connections.flatMap(item => item.requests).find(request => request.userId === value)?.name ?? '' })
  person.value = ''
}

async function save() {
  // The package's derived values must be in the form before it is saved.
  if (completing.value)
    return
  await action(async () => {
    const agent = state.agents.find(item => item.id === form.value.agentId)
    const body = {
      name: form.value.name,
      // The save consumes a completed sign-in.
      ...credentialInput(form.value.token, signIn.flow.value),
      settings: pluginSettings(storedFields.value, values.value),
      // Blank secret fields keep what is saved.
      secrets: pluginSecrets(secrets.value),
      // Agents are granted on their own page; omitting the list keeps it.
      ...(delegates.value && {
        agentId: form.value.agentId,
        // An agent with one project answers in it; a broader one answers outside projects.
        projectId: agent?.access.projects?.length === 1 ? agent.access.projects[0] : null,
        accessMode: form.value.accessMode,
        userIds: people.value.map(item => item.id),
        people: people.value,
        maxMessagesPerHour: Number(form.value.maxMessagesPerHour),
        maxPerPersonPerHour: Number(form.value.maxPerPersonPerHour),
      }),
      enabled: form.value.enabled,
      ...sharing.value,
      // New instances grant the chosen agents; editing keeps current grants.
      ...(!editing.value && grants.value.length ? { agentIds: grants.value } : {}),
    }
    const path = `/plugins/${id.value}/connections${editing.value ? `/${editing.value.id}` : ''}`
    await pluginOperation(path, body, editing.value ? 'PUT' : 'POST')
    signIn.consumed()
    form.value.token = ''
    secrets.value = pluginFormSecrets()
    editing.value = undefined
    await load()
    notify(`${singular.value} saved`)
  })
}

async function post(connection: PluginConnection, path: string, body?: unknown) {
  await action(async () => {
    await api(`/plugins/${id.value}/connections/${connection.id}/${path}`, { method: 'POST', body: body ? JSON.stringify(body) : undefined })
    await load()
  })
}

async function test(connection: PluginConnection) {
  await action(async () => {
    const result = await pluginOperation<{ message?: string }>(`/plugins/${id.value}/connections/${connection.id}/test`)
    notify(`${connection.name}: ${result.message || 'it works'}`)
  })
}

async function disconnect(connection: PluginConnection) {
  await action(async () => {
    const result = await api<{ message: string }>(`/plugins/${id.value}/connections/${connection.id}/disconnect`, { method: 'POST' })
    await load()
    notify(`${connection.name}: ${result.message}`)
  })
}

async function remove() {
  await action(async () => {
    await api(`/plugins/${id.value}/connections/${removing.value!.id}`, { method: 'DELETE' })
    removing.value = undefined
    await load()
  })
}

async function copyCode() {
  const code = signIn.flow.value?.userCode
  if (!code)
    return
  try {
    await navigator.clipboard.writeText(code)
    copied.value = true
  }
  catch {
    copied.value = false
  }
}

function closeEditor() {
  clearTimeout(typing)
  void signIn.cancel()
  asked++
  secrets.value = pluginFormSecrets()
  form.value.token = ''
  completion.value = null
  editing.value = undefined
}

onMounted(async () => {
  await action(load)
  timer = setInterval(() => {
    if (!busy.value && editing.value === undefined)
      void load().catch(() => {})
  }, 5000)
})
onBeforeUnmount(() => {
  clearInterval(timer)
  clearTimeout(typing)
  void signIn.cancel()
  form.value.token = ''
  secrets.value = pluginFormSecrets()
})
</script>

<template>
  <div class="mx-auto w-full max-w-260 px-8 pb-16 pt-8 phone:px-4 phone:pb-32 phone:pt-[max(18px,env(safe-area-inset-top))]">
    <nav class="flex items-center gap-1.5 text-xs font-medium text-muted" aria-label="Breadcrumb">
      <RouterLink to="/atelier" class="hover:text-ink phone:hidden">
        Atelier
      </RouterLink><Icon :name="ChevronRight" :size="12" class="phone:hidden" />
      <RouterLink to="/plugins" class="hover:text-ink">
        Plugins
      </RouterLink><Icon :name="ChevronRight" :size="12" /><span class="text-ink">{{ manifest?.name ?? id }}</span>
    </nav>
    <UiAlert v-if="error && editing === undefined" class="mt-5">
      {{ error }}
    </UiAlert>
    <p v-if="loaded && !entry" class="mt-6 text-sm text-muted">
      This plugin isn’t installed.
    </p>
    <div v-else-if="entry && !manifest" class="mt-6 flex items-center gap-3 rounded-2xl bg-danger-surface px-4 py-3 text-xs text-danger" role="status">
      <Icon :name="TriangleAlert" :size="17" />
      <span class="min-w-0 flex-1">No version of this plugin can be used. Review it to see why.</span>
      <UiButton size="small" class="bg-surface" @click="reviewing = true">
        Review
      </UiButton>
    </div>
    <template v-if="entry && manifest">
      <div class="mt-4.5 flex items-center justify-between gap-4 phone:flex-wrap">
        <div class="flex items-center gap-4">
          <PluginMark :plugin="entry.id" :brand="manifest.brand" :size="56" />
          <div>
            <h1 class="font-heading text-[26px]! font-extrabold leading-none! tracking-[-0.03em]!">
              {{ manifest.name }}
            </h1>
            <p class="mt-2 flex flex-wrap items-center gap-2 text-2xs text-muted">
              <span class="rounded-full bg-hover px-2 py-0.5 font-semibold">{{ entry.builtin ? 'Built-in' : 'Installed' }}</span>
              v{{ entry.active ?? manifest.version }} · Luau sandbox
            </p>
          </div>
        </div>
        <div class="flex flex-wrap gap-2">
          <UiButton @click="reviewing = true">
            <Icon :name="ShieldCheck" :size="15" />Access and versions
          </UiButton>
          <UiButton v-if="entry.active" variant="primary" @click="edit(null)">
            <Icon :name="Plus" :size="15" />Add {{ singular.toLowerCase() }}
          </UiButton>
        </div>
      </div>
      <p class="mt-4 max-w-150 text-sm leading-relaxed text-muted">
        {{ manifest.description }}
      </p>
      <div class="mt-3 flex flex-wrap gap-1.5">
        <span v-for="name in manifest.capabilities" :key="name" class="inline-flex h-6 items-center gap-1.25 rounded-[7px] border border-line bg-inset px-2.25 text-2xs font-medium text-muted">
          <Icon :name="capabilityLabels[name]?.[0] ?? ShieldCheck" :size="12" />{{ capabilityLabels[name]?.[1] ?? name }}
        </span>
        <span v-for="[icon, label] in contributions(manifest)" :key="label" class="inline-flex h-6 items-center gap-1.25 rounded-[7px] border border-line bg-inset px-2.25 text-2xs font-medium text-muted">
          <Icon :name="icon" :size="12" />{{ label }}
        </span>
      </div>
      <div v-if="entry.status !== 'active'" class="mt-5 flex items-center gap-3 rounded-2xl bg-warning-surface px-4 py-3 text-xs text-warning" role="status">
        <Icon :name="TriangleAlert" :size="17" />
        <span class="flex-1">{{ entry.status === 'update' ? 'A new version is waiting for your review.' : 'Review and approve this plugin before adding an instance.' }}</span>
        <UiButton size="small" class="bg-surface" @click="reviewing = true">
          Review
        </UiButton>
      </div>

      <div class="mt-1 grid items-start gap-5" :class="manifest.agentAccess ? 'grid-cols-[minmax(0,1fr)_300px] tablet:grid-cols-1' : ''">
        <div class="min-w-0">
          <section v-if="loaded && !connections.length && entry.active" class="mt-5 grid gap-4 rounded-2xl border border-line bg-surface p-5" aria-labelledby="plugin-first">
            <header class="flex items-start justify-between gap-3 phone:flex-wrap">
              <div class="min-w-0">
                <h2 id="plugin-first" class="text-[15px]! font-bold">
                  Set up your first {{ singular.toLowerCase() }}
                </h2>
                <p class="mt-1 max-w-130 text-xs leading-relaxed text-muted">
                  {{ setup.summary || `No ${plural.toLowerCase()} yet. Add one to start using this plugin.` }}
                </p>
              </div>
              <span v-if="setup.effort" class="inline-flex h-5.5 shrink-0 items-center rounded-full px-2.25 text-2xs font-semibold" :class="setup.effort === 'app' ? 'bg-warning-surface text-warning' : 'bg-soft text-accent'">{{ effortLabels[setup.effort] }}</span>
            </header>
            <ol class="grid" aria-label="Setup steps">
              <li v-for="(item, index) in steps" :key="item.id" class="grid grid-cols-[auto_minmax(0,1fr)] items-start gap-3 border-t border-line py-2.5 first:border-0">
                <span class="grid size-6 place-items-center rounded-full border border-control text-2xs font-bold text-muted">{{ index + 1 }}</span>
                <span class="min-w-0">
                  <span class="flex flex-wrap items-center gap-1.5 text-xs font-semibold">{{ item.title }}<span v-if="item.shared" class="rounded-full bg-hover px-1.75 py-px text-3xs font-semibold text-muted">Once</span></span>
                  <span v-if="item.description" class="mt-0.5 block text-2xs leading-relaxed text-muted">{{ item.description }}</span>
                </span>
              </li>
            </ol>
            <UiButton variant="primary" class="justify-self-start" @click="edit(null)">
              Start setup<Icon :name="ChevronRight" :size="15" />
            </UiButton>
          </section>
          <article
            v-for="connection in connections"
            :key="connection.id"
            class="mt-5 grid gap-4 rounded-2xl border border-line bg-surface p-5"
            :aria-label="connection.name"
          >
            <header class="flex items-center justify-between gap-3">
              <h2 class="truncate text-[15px]! font-bold">
                {{ connection.name }}
              </h2>
              <span class="inline-flex h-5.5 items-center gap-1.5 rounded-full px-2.25 text-2xs font-semibold" :class="tones[connectionState(connection).tone]" role="status">
                <span class="size-1.75 rounded-full bg-current" />{{ connectionState(connection).label }}
              </span>
            </header>
            <p v-if="connection.status.message" class="-mt-2 text-xs" :class="connection.status.state === 'needs_review' ? 'text-coral' : 'text-muted'">
              {{ connection.status.message }}
            </p>
            <dl v-if="shownFields.length || secretFields.length || delegates" class="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-3">
              <div v-for="field in shownFields" :key="field.key" class="rounded-xl border border-line bg-inset px-3.5 py-2.5">
                <dt class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">
                  {{ field.title }}
                </dt>
                <dd class="mt-1 truncate text-xs font-semibold">
                  {{ display(connection.settings[field.key]) }}
                </dd>
              </div>
              <div v-for="field in secretFields" :key="field.key" class="rounded-xl border border-line bg-inset px-3.5 py-2.5">
                <dt class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">
                  {{ field.title }}
                </dt>
                <dd class="mt-1 flex items-center gap-1.5 text-xs font-semibold" :class="connection.secretKeys?.includes(field.key) ? '' : 'text-coral'">
                  <Icon :name="KeyRound" :size="12" />{{ connection.secretKeys?.includes(field.key) ? 'Saved' : 'Not set' }}
                </dd>
              </div>
              <div v-if="delegates" class="rounded-xl border border-line bg-inset px-3.5 py-2.5">
                <dt class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">
                  Answers as
                </dt>
                <dd class="mt-1 truncate text-xs font-semibold">
                  {{ state.agents.find(agent => agent.id === connection.agentId)?.name ?? 'Missing agent' }}<template v-if="connection.projectId">
                    · {{ state.projects.find(project => project.id === connection.projectId)?.name ?? 'Missing project' }}
                  </template>
                </dd>
              </div>
              <div v-if="delegates" class="rounded-xl border border-line bg-inset px-3.5 py-2.5">
                <dt class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">
                  This hour
                </dt>
                <dd class="mt-1 text-xs font-semibold">
                  {{ connection.usage.hour }} / {{ connection.maxMessagesPerHour }} messages
                  <span
                    class="mt-1.5 block h-1.5 overflow-hidden rounded-full bg-hover"
                    role="meter"
                    aria-label="Messages this hour"
                    :aria-valuenow="connection.usage.hour"
                    aria-valuemin="0"
                    :aria-valuemax="connection.maxMessagesPerHour"
                  ><span class="block h-full rounded-full bg-accent" :style="{ width: `${Math.min(100, connection.usage.hour / connection.maxMessagesPerHour * 100)}%` }" /></span>
                </dd>
              </div>
            </dl>
            <p v-if="manifest.agentAccess" class="text-xs text-muted">
              <template v-if="agentNames(connection).length">
                Authorized agents: <b class="text-ink">{{ agentNames(connection).join(', ') }}</b>
              </template>
              <template v-else>
                No agent can use it yet.
              </template>
              <RouterLink to="/agents" class="ml-1 text-accent hover:underline">
                Manage on agent pages ›
              </RouterLink>
            </p>
            <section v-if="delegates">
              <h3 class="text-xs! font-semibold">
                <template v-if="connection.accessMode === 'everyone'">
                  Anyone · {{ connection.maxPerPersonPerHour ?? 5 }}/hour each
                </template>
                <template v-else>
                  {{ connection.userIds.length }} {{ connection.userIds.length === 1 ? 'person' : 'people' }} · {{ connection.accessMode === 'requests' ? 'approve as they ask' : 'only listed people' }}
                </template>
              </h3>
              <ul class="mt-2 grid gap-1.5">
                <li v-for="request in connection.requests" :key="request.id" class="flex items-center gap-3 rounded-xl bg-coral-soft px-3 py-2">
                  <PersonAvatar :name="request.name || request.userId" :identity="request.userId" :size="26" />
                  <span class="flex-1 truncate text-xs"><b>{{ request.name || request.userId }}</b> asked to use it</span>
                  <UiButton size="small" :disabled="busy" @click="post(connection, `people/${request.userId}/deny`)">
                    Deny
                  </UiButton>
                  <UiButton
                    size="small"
                    variant="primary"
                    :disabled="busy"
                    @click="post(connection, `people/${request.userId}/approve`)"
                  >
                    Approve
                  </UiButton>
                </li>
                <li v-for="user in connection.userIds" :key="user" class="flex items-center gap-3 px-1 py-1">
                  <PersonAvatar :name="personName(connection, user)" :identity="user" :size="26" />
                  <span class="flex-1 truncate text-xs">{{ personName(connection, user) }} <span class="font-mono text-3xs text-muted">{{ user }}</span></span>
                  <UiButton
                    size="small"
                    :disabled="busy"
                    :aria-label="`Remove ${personName(connection, user)}`"
                    @click="post(connection, `people/${user}/remove`)"
                  >
                    Remove
                  </UiButton>
                </li>
              </ul>
            </section>
            <section v-if="delegates" :aria-label="`Recent activity of ${connection.name}`">
              <h3 class="text-xs! font-semibold">
                Recent activity
              </h3>
              <p v-if="!connection.activity.length" class="mt-2 text-xs text-muted">
                Nothing yet.
              </p>
              <ul v-else class="mt-2 overflow-hidden rounded-xl border border-line">
                <li v-for="item in connection.activity" :key="item.id" class="border-t border-line first:border-0">
                  <RouterLink :to="`/chats/${item.chatId}`" class="flex items-center gap-3 px-3.5 py-2.5 hover:bg-hover/40">
                    <PersonAvatar :name="item.name || item.userId" :identity="item.userId" :size="26" />
                    <span class="min-w-0 flex-1">
                      <span class="block truncate text-xs font-semibold">{{ item.name || item.userId }} <span class="font-normal text-muted">· {{ relative(item.createdAt) }}</span></span>
                      <span class="block truncate text-2xs text-muted">“{{ item.preview }}”</span>
                    </span>
                    <span class="inline-flex h-5.5 items-center rounded-full px-2.25 text-2xs font-semibold" :class="activityTones[item.status][0]">{{ activityTones[item.status][1] }}</span>
                  </RouterLink>
                </li>
              </ul>
            </section>
            <PluginInstancePanel
              v-if="manifest.instanceView"
              :plugin="id"
              :instance="connection.id"
              :name="connection.name"
            />
            <footer class="flex flex-wrap items-center justify-between gap-3 border-t border-line pt-4">
              <label class="flex flex-row! items-center gap-2! text-xs text-ink! font-normal!">
                <input
                  type="checkbox"
                  role="switch"
                  :checked="connection.enabled"
                  :disabled="busy"
                  @change="post(connection, 'enabled', { enabled: !connection.enabled })"
                >
                Active
              </label>
              <span class="flex flex-wrap gap-2">
                <UiButton
                  v-if="manifest.credential?.oauth"
                  size="small"
                  :disabled="busy"
                  @click="disconnect(connection)"
                >
                  Disconnect
                </UiButton>
                <UiButton
                  v-if="manifest.instanceTest"
                  size="small"
                  :disabled="busy"
                  :aria-label="`Test ${connection.name}`"
                  @click="test(connection)"
                >
                  <Icon :name="Zap" :size="13" />Test
                </UiButton>
                <UiButton size="small" :aria-label="`Edit ${connection.name}`" @click="edit(connection)">
                  <Icon :name="Pencil" :size="13" />Edit
                </UiButton>
                <UiButton
                  size="small"
                  variant="danger-outline"
                  :aria-label="`Delete ${connection.name}`"
                  @click="removing = connection"
                >
                  <Icon :name="Trash2" :size="13" />Delete
                </UiButton>
              </span>
            </footer>
          </article>
        </div>

        <aside v-if="manifest.agentAccess" class="mt-5 grid min-w-0 grid-cols-[minmax(0,1fr)] gap-3 rounded-2xl border border-line bg-surface p-4.5" aria-labelledby="plugin-agents">
          <h2 id="plugin-agents" class="flex items-center gap-2 text-[13px]! font-semibold">
            <Icon :name="Terminal" :size="16" class="text-accent" />How agents use it
          </h2>
          <p class="text-xs leading-relaxed text-muted">
            Only agents granted an active {{ singular.toLowerCase() }} on their page get these, in their conversations.
          </p>
          <div v-if="manifest.commands?.length" class="grid min-w-0 grid-cols-[minmax(0,1fr)] gap-2">
            <h3 class="text-3xs! font-semibold uppercase tracking-[.12em] text-subtle">
              Commands
            </h3>
            <div v-for="command in manifest.commands" :key="command.name">
              <pre class="whitespace-pre-wrap break-words rounded-lg border border-line bg-inset px-2.5 py-2 font-mono text-3xs"><code>{{ commandUsage(manifest.id, command) }}</code></pre>
              <p v-if="command.description" class="mt-1 text-2xs text-muted">
                {{ command.description }}<template v-if="command.output === 'secret'">
                  The value goes to the command's output only, never to the conversation.
                </template>
              </p>
            </div>
          </div>
          <div v-if="manifest.tools?.length" class="grid min-w-0 gap-1.5">
            <h3 class="text-3xs! font-semibold uppercase tracking-[.12em] text-subtle">
              Tools
            </h3>
            <p v-for="tool in manifest.tools" :key="tool.name" class="flex items-start gap-2 text-2xs text-muted">
              <Icon :name="Wrench" :size="12" class="mt-px shrink-0" /><span><b class="font-mono text-ink">{{ tool.name }}</b></span>
            </p>
          </div>
          <div v-if="manifest.programs?.length" class="grid min-w-0 gap-1.5 break-words">
            <h3 class="text-3xs! font-semibold uppercase tracking-[.12em] text-subtle">
              Programs
            </h3>
            <p v-for="program in manifest.programs" :key="program.name" class="text-2xs text-muted">
              <b class="font-mono text-ink">{{ program.path || program.name }}</b><template v-if="program.description">
                · {{ program.description }}
              </template>
            </p>
          </div>
        </aside>
      </div>
    </template>

    <PluginPrograms
      v-if="entry?.active && manifest?.programs?.some(program => program.installer)"
      :key="`${id}:${entry.active}`"
      :plugin="id"
      class="mt-5"
    />

    <PluginReview
      v-if="reviewing && entry"
      :entry="entry"
      :capabilities="catalog.capabilities"
      @close="reviewing = false"
      @changed="load"
    />

    <Modal
      v-if="editing !== undefined && manifest && current"
      wide
      :title="editing ? `Edit ${editing.name}` : manifest.instances ? `New ${singular.toLowerCase()}` : `New ${manifest.name} instance`"
      @close="closeEditor"
    >
      <form class="grid grid-cols-[190px_minmax(0,1fr)] phone:grid-cols-1" @submit.prevent="next">
        <nav class="border-r border-line bg-inset px-2.5 py-5 phone:border-r-0 phone:border-b phone:px-4 phone:py-3" aria-label="Setup steps">
          <ol class="grid gap-1 phone:flex phone:gap-1.5 phone:overflow-x-auto">
            <li v-for="(item, index) in steps" :key="item.id" class="phone:shrink-0">
              <button
                type="button"
                class="grid w-full grid-cols-[auto_minmax(0,1fr)] items-center gap-2.5 rounded-xl px-2.5 py-2 text-left text-xs enabled:hover:bg-hover disabled:opacity-50"
                :class="index === step ? 'bg-surface font-semibold shadow-arcade' : ''"
                :aria-current="index === step ? 'step' : undefined"
                :disabled="!reachable(index)"
                @click="step = index"
              >
                <span
                  aria-hidden="true"
                  class="grid size-6 place-items-center rounded-full border text-2xs font-bold"
                  :class="index < step && !stepProblem(item) ? 'border-success bg-success text-white' : index === step ? 'border-accent text-accent' : 'border-control text-muted'"
                ><Icon v-if="index < step && !stepProblem(item)" :name="Check" :size="12" /><template v-else>{{ index + 1 }}</template></span>
                <span class="min-w-0 truncate"><span class="sr-only">Step {{ index + 1 }}: </span>{{ item.title }}</span>
              </button>
            </li>
          </ol>
        </nav>
        <div class="grid min-w-0 content-start gap-5 px-6.5 py-6 phone:p-5">
          <UiAlert v-if="error">
            {{ error }}
          </UiAlert>
          <UiAlert v-if="completion?.errors.form">
            {{ completion.errors.form }}
          </UiAlert>
          <header>
            <p class="text-3xs font-semibold uppercase tracking-[.12em] text-subtle">
              Step {{ step + 1 }} of {{ steps.length }}
            </p>
            <h3 class="mt-1 text-[17px]! font-bold">
              {{ current.title }}
            </h3>
            <p v-if="current.description" class="mt-1 text-xs leading-relaxed text-muted">
              {{ current.description }}
            </p>
          </header>

          <fieldset v-if="current.shared && sources.length && !editing" class="grid gap-1.5" :disabled="busy">
            <legend class="sr-only">
              Which {{ current.title }}
            </legend>
            <label
              v-for="source in sources"
              :key="source.id"
              class="grid! grid-cols-[auto_1fr] items-start gap-x-2.5! gap-y-0.5! rounded-xl border border-line px-3.5 py-2.5 font-normal!"
              :class="reuse === source.id ? 'border-accent bg-soft' : 'hover:bg-hover/40'"
            >
              <input
                type="radio"
                class="mt-0.5 w-auto!"
                name="plugin-shared-source"
                :checked="reuse === source.id"
                @change="chooseSource(source.id)"
              ><span class="text-xs font-semibold">Use the same one as {{ source.names.join(', ') }}</span>
              <small v-if="source.label" class="col-start-2 font-mono">{{ shortValue(source.label, 14) }}</small>
            </label>
            <label
              class="grid! grid-cols-[auto_1fr] items-start gap-x-2.5! gap-y-0.5! rounded-xl border border-line px-3.5 py-2.5 font-normal!"
              :class="!reuse ? 'border-accent bg-soft' : 'hover:bg-hover/40'"
            >
              <input
                type="radio"
                class="mt-0.5 w-auto!"
                name="plugin-shared-source"
                :checked="!reuse"
                @change="chooseSource('')"
              ><span class="text-xs font-semibold">Set up another one</span>
            </label>
          </fieldset>

          <template v-if="!(current.shared && reuse)">
            <PluginChecklist
              v-if="current.checklist?.length"
              :key="current.id"
              :tasks="current.checklist"
              :callback-url="oauthCallbackUrl"
            />
            <p v-if="current.warning" class="flex items-start gap-2.5 rounded-xl bg-warning-surface px-4 py-3 text-xs leading-relaxed text-warning" role="note">
              <Icon :name="TriangleAlert" :size="15" class="mt-px shrink-0" />{{ current.warning }}
            </p>
          </template>

          <template v-if="current.kind === 'settings' && !(current.shared && reuse)">
            <PluginPresets
              v-if="current.presets?.length"
              v-model:values="values"
              :step="current"
              :fields="fields"
              :disabled="busy"
              @refresh="presetChanged"
            />
            <PluginFields
              v-if="stepShown(current).length"
              v-model:values="values"
              v-model:secrets="secrets"
              :fields="stepShown(current)"
              :saved="editing?.secretKeys ?? []"
              :disabled="busy"
              :options="completion?.options"
              :errors="formErrors"
              @refresh="complete"
            />
            <details v-if="current.presets?.length && switches(current).length" class="rounded-xl border border-line px-4 py-3" :open="!matchingPreset(current, fields, values)">
              <summary class="cursor-pointer text-xs font-semibold">
                Fine-tune
              </summary>
              <PluginFields
                v-model:values="values"
                v-model:secrets="secrets"
                class="mt-3"
                :fields="switches(current)"
                :disabled="busy"
                :errors="formErrors"
                @refresh="complete"
              />
            </details>
          </template>

          <template v-if="current.kind === 'credential'">
            <label v-if="manifest.credential && !manifest.credential.oauth">{{ editing ? `New ${manifest.credential.label.toLowerCase()} (optional)` : manifest.credential.label }}<input
              v-model="form.token"
              type="password"
              autocomplete="new-password"
              spellcheck="false"
              :required="!editing && !signIn.completed.value"
              :placeholder="signIn.completed.value ? 'Signed in · or paste a token instead' : editing ? 'Saved · leave blank to keep' : ''"
              :aria-invalid="Boolean(completion?.errors.credential)"
              @input="typed()"
            ><small :class="completion?.errors.credential ? 'text-coral' : ''">{{ completion?.errors.credential || 'Stored encrypted. Cairn adds it to this plugin’s requests; the plugin never sees it.' }}</small></label>
            <UiAlert v-if="manifest.credential?.oauth && completion?.errors.credential">
              {{ completion.errors.credential }}
            </UiAlert>
            <section v-if="manifest.credential?.deviceFlow || manifest.credential?.oauth" class="grid gap-3" :aria-label="current.title">
              <label v-if="manifest.credential?.oauth && oauthCallbackUrl && !callbackShown">Callback URL<input :value="oauthCallbackUrl" readonly><small>Register this address in your provider’s OAuth application before signing in.</small></label>
              <UiButton
                v-if="!signIn.flow.value || signIn.completed.value"
                class="justify-self-start"
                :variant="signIn.completed.value ? 'default' : 'primary'"
                :disabled="busy || signIn.starting.value"
                @click="signIn.start()"
              >
                <Icon :name="KeyRound" :size="15" />{{ signIn.completed.value ? 'Sign in with another account' : editing ? 'Sign in again' : current.title }}
              </UiButton>
              <UiAlert v-if="signIn.error.value || signInProblem(signIn.flow.value)">
                {{ signIn.error.value || signInProblem(signIn.flow.value) }}
              </UiAlert>
              <p v-if="signIn.completed.value" class="flex items-center gap-1.5 text-xs font-semibold text-success" role="status">
                <Icon :name="Check" :size="14" />{{ last ? `Signed in. Save to keep this ${singular.toLowerCase()}.` : 'Signed in.' }}
              </p>
              <div v-else-if="signIn.pending.value && signIn.flow.value" class="grid gap-3 rounded-2xl bg-inset p-4" role="status">
                <p class="text-sm">
                  {{ manifest.credential?.oauth ? `Approve access in the page that opened, then come back here. Your password stays with the provider.` : `Open ${manifest.name} and enter this code. Your password stays with ${manifest.name}.` }}
                </p>
                <div v-if="signIn.flow.value.userCode" class="flex flex-wrap items-center gap-2">
                  <code aria-label="Verification code" class="select-all font-mono text-2xl font-bold tracking-widest">{{ signIn.flow.value.userCode }}</code>
                  <UiButton size="small" :aria-label="copied ? 'Code copied' : 'Copy verification code'" @click="copyCode">
                    <Icon :name="copied ? Check : Copy" :size="13" />{{ copied ? 'Copied' : 'Copy' }}
                  </UiButton>
                </div>
                <p v-if="signIn.flow.value.message" class="text-xs text-muted">
                  {{ signIn.flow.value.message }}
                </p>
                <div class="flex flex-wrap gap-2">
                  <a
                    v-if="verificationLink(signIn.flow.value)"
                    :class="[buttonBase, buttonVariants.primary, buttonSizes.small]"
                    :href="verificationLink(signIn.flow.value)"
                    target="_blank"
                    rel="noopener noreferrer"
                  >Open {{ manifest.credential?.oauth ? 'sign-in' : manifest.name }}<Icon :name="ArrowUpRight" :size="14" /></a>
                  <UiButton size="small" @click="signIn.cancel()">
                    Cancel sign-in
                  </UiButton>
                </div>
              </div>
              <UiButton
                v-else-if="signInProblem(signIn.flow.value)"
                size="small"
                class="justify-self-start"
                :disabled="busy || signIn.starting.value"
                @click="signIn.start()"
              >
                Try again
              </UiButton>
            </section>
          </template>

          <div
            v-if="guided(current) && (completion?.view.length || completing)"
            class="grid gap-2.5"
            :aria-busy="completing"
            aria-live="polite"
          >
            <p v-if="completing && !completion?.view.length" class="text-2xs text-muted" role="status">
              Checking…
            </p>
            <PluginView :nodes="completion?.view ?? []" :busy="completing" />
          </div>

          <template v-if="current.kind === 'people' && delegates">
            <fieldset class="grid gap-2" :disabled="busy">
              <legend class="mb-1 text-xs font-semibold">
                Who can talk to it
              </legend>
              <label class="grid! grid-cols-[auto_1fr] items-start gap-x-2! gap-y-0.5!"><input
                v-model="form.accessMode"
                type="radio"
                value="requests"
                class="mt-0.5 w-auto!"
              >Approve people as they ask<small class="col-start-2 font-normal">Unknown people get a request you approve here; their messages aren’t kept until then.</small></label>
              <label class="grid! grid-cols-[auto_1fr] items-start gap-x-2! gap-y-0.5!"><input
                v-model="form.accessMode"
                type="radio"
                value="allowlist"
                class="mt-0.5 w-auto!"
              >Only people I add<small class="col-start-2 font-normal">Everyone else is ignored without a trace.</small></label>
              <label class="grid! grid-cols-[auto_1fr] items-start gap-x-2! gap-y-0.5!"><input
                v-model="form.accessMode"
                type="radio"
                value="everyone"
                class="mt-0.5 w-auto!"
              >Anyone<small class="col-start-2 font-normal">Everyone who can post in the channel, within an hourly limit per person.</small></label>
            </fieldset>
            <fieldset class="grid gap-2" :disabled="busy">
              <legend class="mb-1 text-xs font-semibold">
                {{ form.accessMode === 'everyone' ? 'Already allowed' : 'Allowed now' }}
              </legend>
              <ul v-if="people.length" class="grid gap-1.5">
                <li v-for="item in people" :key="item.id" class="flex items-center gap-3 rounded-xl border border-line px-3 py-1.5">
                  <PersonAvatar :name="item.name || item.id" :identity="item.id" :size="24" />
                  <span class="min-w-0 flex-1 truncate text-xs">{{ item.name || 'Person' }} <span class="font-mono text-3xs text-muted">{{ item.id }}</span></span>
                  <UiButton size="small" :aria-label="`Remove ${item.name || item.id}`" @click="people = people.filter(other => other.id !== item.id)">
                    <Icon :name="X" :size="13" />
                  </UiButton>
                </li>
              </ul>
              <div class="flex gap-2">
                <input
                  v-model="person"
                  aria-label="Person’s ID"
                  placeholder="Add a person’s ID"
                  class="min-w-0 flex-1"
                  @keydown.enter.prevent="addPerson"
                >
                <UiButton :disabled="!person.trim()" @click="addPerson">
                  Add
                </UiButton>
              </div>
            </fieldset>
            <fieldset class="grid gap-1.5" :disabled="busy">
              <legend class="mb-1 text-xs font-semibold">
                Answers as
              </legend>
              <label
                v-for="agent in agents"
                :key="agent.id"
                class="grid! grid-cols-[auto_1fr_auto] items-center gap-x-2.5! rounded-xl border border-line px-3.5 py-2 font-normal!"
                :class="form.agentId === agent.id ? 'border-accent bg-soft' : ''"
              >
                <input
                  v-model="form.agentId"
                  type="radio"
                  class="w-auto!"
                  :value="agent.id"
                ><span class="truncate text-xs font-semibold">{{ agent.name }}</span>
                <span v-if="risks(agent).length" class="rounded-full bg-warning-surface px-2 py-px text-3xs font-semibold text-warning">Broad access</span>
              </label>
              <small>People reach this agent with all of its access. <RouterLink to="/agents" class="text-accent">Create a dedicated agent</RouterLink> to limit what they can do.</small>
            </fieldset>
            <label>Messages per hour<input
              v-model="form.maxMessagesPerHour"
              type="number"
              min="1"
              max="100"
            ></label>
            <label v-if="form.accessMode === 'everyone'">Messages per person per hour<input
              v-model="form.maxPerPersonPerHour"
              type="number"
              min="1"
              max="50"
            ></label>
          </template>

          <template v-if="current.kind === 'agents' && manifest.agentAccess">
            <p v-if="manifest.agentAccess.description" class="text-xs leading-relaxed text-muted">
              {{ manifest.agentAccess.description }}
            </p>
            <fieldset v-if="!editing" class="grid gap-1.5" :disabled="busy">
              <legend class="mb-1 text-xs font-semibold">
                Agents that can use it
              </legend>
              <label
                v-for="agent in state.agents"
                :key="agent.id"
                class="grid! grid-cols-[auto_1fr_auto] items-center gap-x-2.5! rounded-xl border border-line px-3.5 py-2 font-normal!"
                :class="holder(agent) ? 'opacity-60' : grants.includes(agent.id) ? 'border-accent bg-soft' : ''"
              >
                <input
                  type="checkbox"
                  class="w-auto!"
                  :checked="grants.includes(agent.id)"
                  :disabled="Boolean(holder(agent))"
                  @change="toggleGrant(agent.id, ($event.target as HTMLInputElement).checked)"
                ><span class="truncate text-xs font-semibold">{{ agent.name }}</span>
                <span v-if="holder(agent)" class="rounded-full bg-hover px-2 py-px text-3xs font-semibold text-muted">Uses {{ holder(agent) }}</span>
              </label>
              <small>You can skip this: no agent can use it until you grant one here or on the agent’s page.</small>
            </fieldset>
            <section v-else class="grid gap-1.5 rounded-xl border border-line bg-inset px-4 py-3" aria-label="Agents">
              <p class="text-xs">
                <template v-if="agentNames(editing).length">
                  Authorized agents: <b>{{ agentNames(editing).join(', ') }}</b>
                </template>
                <template v-else>
                  No agent can use it yet.
                </template>
              </p>
              <p class="text-2xs text-muted">
                Change it from the Secrets &amp; plugins section of each agent. <RouterLink to="/agents" class="text-accent hover:underline">
                  Manage on agent pages ›
                </RouterLink>
              </p>
            </section>
          </template>

          <div v-if="delegates && ['people', 'agents'].includes(current.kind) && chosenAgent && chosenRisks.length" class="flex gap-2.5 rounded-xl bg-warning-surface px-3.5 py-2.5 text-xs leading-relaxed text-warning" role="status">
            <Icon :name="TriangleAlert" :size="15" class="mt-0.5 shrink-0" />
            <p>
              <b class="block">{{ form.accessMode === 'everyone' ? 'Anyone in the channel' : 'The people you allow' }} can use everything {{ chosenAgent.name }} can do</b>
              Through the bot, they can {{ listed(chosenRisks) }}.
              {{ form.accessMode === 'everyone' ? 'For a public channel, prefer a read-only agent with one project.' : 'Only allow people you trust, or choose a more limited agent.' }}
            </p>
          </div>

          <template v-if="last || editing">
            <hr class="border-line">
            <label>Name<input
              v-model="form.name"
              required
              maxlength="100"
              @input="named = true"
            ></label>
            <label class="flex flex-row! items-center gap-2!"><input v-model="form.enabled" type="checkbox" class="w-auto!">Active</label>
          </template>

          <footer class="flex flex-wrap items-center justify-between gap-2.5 border-t border-line pt-4">
            <span class="min-w-0 flex-1 text-2xs text-muted" role="status">{{ !editing && !last ? stepProblem(current) : '' }}</span>
            <span class="flex flex-wrap justify-end gap-2.5">
              <UiButton v-if="step > 0" :disabled="busy" @click="step--">
                Back
              </UiButton>
              <UiButton v-else :disabled="busy" @click="closeEditor">
                Cancel
              </UiButton>
              <UiButton v-if="editing && !last" :disabled="busy" @click="step++">
                Next
              </UiButton>
              <UiButton
                type="submit"
                variant="primary"
                :disabled="busy || completing || (editing || last ? steps.some(item => stepProblem(item)) : Boolean(stepProblem(current)))"
              >
                {{ editing || last ? `Save ${singular.toLowerCase()}` : 'Continue' }}
              </UiButton>
            </span>
          </footer>
        </div>
      </form>
    </Modal>
    <Modal v-if="removing" :title="`Delete ${removing.name}?`" @close="removing = undefined">
      <div class="px-6.5 py-6 phone:p-5">
        <p class="text-sm text-muted">
          It stops, and its secrets, stored values and timers are deleted. Agents lose access immediately. Conversations stay in Cairn.
        </p>
      </div>
      <footer class="modal-actions sticky bottom-0 flex justify-end gap-2.5 border-t border-line bg-surface px-6.5 py-4.5">
        <UiButton :disabled="busy" @click="removing = undefined">
          Cancel
        </UiButton>
        <UiButton variant="danger" :disabled="busy" @click="remove">
          Delete {{ singular.toLowerCase() }}
        </UiButton>
      </footer>
    </Modal>
  </div>
</template>
