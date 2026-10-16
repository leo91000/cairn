<script setup lang="ts">
import type { Catalog, PluginConnection, PluginEntry } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
} from 'vue'
import { useRoute } from 'vue-router'
import { api, notify, state } from '../api'
import Icon from '../components/Icon.vue'
import Modal from '../components/Modal.vue'
import PersonAvatar from '../components/PersonAvatar.vue'
import PluginFields from '../components/PluginFields.vue'
import PluginInstancePanel from '../components/PluginInstancePanel.vue'
import PluginMark from '../components/PluginMark.vue'
import PluginReview from '../components/PluginReview.vue'
import UiAlert from '../components/UiAlert.vue'
import UiButton from '../components/UiButton.vue'
import VirtualSelect from '../components/VirtualSelect.vue'
import {
  ChevronRight,
  KeyRound,
  Pencil,
  Plus,
  ShieldCheck,
  Terminal,
  Trash2,
  TriangleAlert,
  Wrench,
  Zap,
} from '../icons'
import {
  capabilityLabels,
  commandUsage,
  connectionState,
  contributions,
  eligibleAgent,
  instanceNoun,
  personName,
} from '../plugins'

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
const secrets = ref<Record<string, string>>({})
let timer: ReturnType<typeof setInterval> | undefined

const entry = computed<PluginEntry | undefined>(() => catalog.value.packages.find(item => item.id === id.value))
const manifest = computed(() => entry.value?.manifest)
const connections = computed(() => catalog.value.connections.filter(item => item.pluginId === id.value))
const delegates = computed(() => manifest.value?.capabilities.includes('conversations.submit') ?? false)
const fields = computed(() => (manifest.value?.settings ?? []).filter(field => !field.hidden))
const plainFields = computed(() => fields.value.filter(field => field.type !== 'secret'))
const secretFields = computed(() => fields.value.filter(field => field.type === 'secret'))
const agents = computed(() => state.agents.filter(eligibleAgent).map(agent => ({ value: agent.id, label: agent.name })))
const singular = computed(() => instanceNoun(manifest.value, 1))
const plural = computed(() => instanceNoun(manifest.value, 2))
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
  editing.value = connection
  values.value = Object.fromEntries(plainFields.value.map(field => [
    field.key,
    connection?.settings[field.key] ?? field.default ?? (field.type === 'boolean' ? false : field.type === 'multiselect' ? [] : ''),
  ]))
  secrets.value = {}
  form.value = {
    name: connection?.name ?? (manifest.value?.instances ? '' : manifest.value?.name ?? ''),
    token: '',
    agentId: connection?.agentId ?? '',
    agentIds: [...(connection?.agentIds ?? [])],
    accessMode: connection?.accessMode ?? 'allowlist',
    userIds: connection?.userIds.join(', ') ?? '',
    maxMessagesPerHour: connection?.maxMessagesPerHour ?? 20,
    enabled: connection?.enabled ?? true,
  }
}

async function save() {
  await action(async () => {
    const settings = Object.fromEntries(Object.entries(values.value)
      .filter(([, value]) => value !== '' && value !== undefined && value !== null)
      .map(([key, value]) => [key, plainFields.value.find(field => field.key === key)?.type === 'number' ? Number(value) : value]))
    const agent = state.agents.find(item => item.id === form.value.agentId)
    const userIds = String(form.value.userIds).split(/[\s,]+/).filter(Boolean)
    const body = {
      name: form.value.name,
      ...(form.value.token ? { token: form.value.token } : {}),
      settings,
      // Blank secret fields keep what is saved.
      secrets: Object.fromEntries(Object.entries(secrets.value).filter(([, value]) => value.trim())),
      ...(manifest.value?.agentAccess && { agentIds: form.value.agentIds }),
      ...(delegates.value && {
        agentId: form.value.agentId,
        projectId: agent?.access.projects?.[0] ?? null,
        accessMode: form.value.accessMode,
        userIds,
        people: userIds.map(user => ({ id: user, name: editing.value ? personName(editing.value, user) : '' })),
        maxMessagesPerHour: Number(form.value.maxMessagesPerHour),
      }),
      enabled: form.value.enabled,
    }
    const path = `/plugins/${id.value}/connections${editing.value ? `/${editing.value.id}` : ''}`
    await api(path, { method: editing.value ? 'PUT' : 'POST', body: JSON.stringify(body) })
    form.value.token = ''
    secrets.value = {}
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
    const result = await api<{ message?: string }>(`/plugins/${id.value}/connections/${connection.id}/test`, { method: 'POST' })
    notify(`${connection.name}: ${result.message || 'it works'}`)
  })
}

async function remove() {
  await action(async () => {
    await api(`/plugins/${id.value}/connections/${removing.value!.id}`, { method: 'DELETE' })
    removing.value = undefined
    await load()
  })
}

function closeEditor() {
  secrets.value = {}
  form.value.token = ''
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
  form.value.token = ''
  secrets.value = {}
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
          <p v-if="loaded && !connections.length && entry.active" class="mt-5 rounded-2xl border border-dashed border-line p-6 text-center text-sm text-muted">
            No {{ plural.toLowerCase() }} yet. Add one to start using this plugin.
          </p>
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
            <dl v-if="plainFields.length || secretFields.length || delegates" class="grid grid-cols-[repeat(auto-fill,minmax(180px,1fr))] gap-3">
              <div v-for="field in plainFields" :key="field.key" class="rounded-xl border border-line bg-inset px-3.5 py-2.5">
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
                  {{ state.agents.find(agent => agent.id === connection.agentId)?.name ?? 'Missing agent' }} · {{ connection.usage.hour }}/{{ connection.maxMessagesPerHour }} this hour
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
            </p>
            <section v-if="delegates">
              <h3 class="text-xs! font-semibold">
                People · {{ connection.accessMode === 'requests' ? 'approve as they ask' : 'only listed people' }}
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
            Only agents you authorize on an active {{ singular.toLowerCase() }} get these, in their conversations.
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
              <b class="font-mono text-ink">{{ program.path }}</b><template v-if="program.description">
                · {{ program.description }}
              </template>
            </p>
          </div>
        </aside>
      </div>
    </template>

    <PluginReview
      v-if="reviewing && entry"
      :entry="entry"
      :capabilities="catalog.capabilities"
      @close="reviewing = false"
      @changed="load"
    />

    <Modal
      v-if="editing !== undefined && manifest"
      :title="editing ? `Edit ${editing.name}` : manifest.instances ? `New ${singular.toLowerCase()}` : `New ${manifest.name} instance`"
      @close="closeEditor"
    >
      <form class="grid gap-5 px-6.5 py-6 phone:p-5" @submit.prevent="save">
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <label>Name<input v-model="form.name" required maxlength="100"></label>
        <label v-if="manifest.credential">{{ manifest.credential.label }}<input
          v-model="form.token"
          type="password"
          autocomplete="new-password"
          :required="!editing"
          :placeholder="editing ? 'Saved · leave blank to keep' : ''"
        ><small>Stored encrypted. Cairn adds it to this plugin’s requests; the plugin never sees it.</small></label>
        <PluginFields
          v-model:values="values"
          v-model:secrets="secrets"
          :fields="fields"
          :saved="editing?.secretKeys ?? []"
          :disabled="busy"
        />
        <fieldset v-if="manifest.agentAccess" class="grid gap-2" :disabled="busy">
          <legend class="mb-1 text-xs font-semibold">
            Authorized agents
          </legend>
          <p v-if="manifest.agentAccess.description" class="text-2xs leading-relaxed text-muted">
            {{ manifest.agentAccess.description }}
          </p>
          <label v-for="agent in state.agents" :key="agent.id" class="flex flex-row! items-center gap-2! font-normal!">
            <input
              v-model="form.agentIds"
              type="checkbox"
              class="w-auto!"
              :value="agent.id"
            >{{ agent.name }}
          </label>
        </fieldset>
        <template v-if="delegates">
          <VirtualSelect
            v-model="form.agentId"
            label="Agent"
            :options="agents"
            placeholder="Choose a dedicated agent"
          />
          <fieldset class="grid gap-2">
            <legend class="mb-1 text-xs font-medium">
              Who can use it
            </legend>
            <label class="flex flex-row! items-center gap-2!"><input
              v-model="form.accessMode"
              type="radio"
              value="requests"
              class="w-auto!"
            >Approve people as they ask</label>
            <label class="flex flex-row! items-center gap-2!"><input
              v-model="form.accessMode"
              type="radio"
              value="allowlist"
              class="w-auto!"
            >Only people listed below</label>
            <label class="flex flex-row! items-center gap-2!"><input
              v-model="form.accessMode"
              type="radio"
              value="everyone"
              class="w-auto!"
            >Anyone (needs a read-only agent without GitHub, MCP or plugin access)</label>
          </fieldset>
          <label>People’s IDs<input v-model="form.userIds" placeholder="Separate IDs with commas"></label>
          <label>Messages per hour<input
            v-model="form.maxMessagesPerHour"
            type="number"
            min="1"
            max="100"
          ></label>
        </template>
        <label class="flex flex-row! items-center gap-2!"><input v-model="form.enabled" type="checkbox" class="w-auto!">Active</label>
        <div class="flex justify-end gap-2.5">
          <UiButton :disabled="busy" @click="closeEditor">
            Cancel
          </UiButton>
          <UiButton type="submit" variant="primary" :disabled="busy">
            Save {{ singular.toLowerCase() }}
          </UiButton>
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
