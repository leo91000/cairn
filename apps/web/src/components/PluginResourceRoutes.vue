<script setup lang="ts">
import type { AccessRequest, PluginRoute, RouteEligibility } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  ref,
  useId,
  watch,
} from 'vue'
import { api, state } from '../api'
import { pluginManagementItems } from '../plugin-management'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ path: string, name: string, enabled: boolean }>()
const emit = defineEmits<{ activity: [active: boolean] }>()
const formId = useId()
const opened = ref(false)
const ready = ref(false)
const busy = ref(false)
const writing = ref(false)
const error = ref('')
const routes = ref<PluginRoute[]>([])
const editing = ref<{ route?: PluginRoute }>()
const removing = ref<PluginRoute>()
const people = ref<{ route: PluginRoute, requests: (AccessRequest & { status: string })[] }>()
const name = ref('')
const enabled = ref(true)
const conditions = ref<{ key: string, value: string }[]>([])
const catchAll = ref(false)
const agent = ref('')
const project = ref('')
const mode = ref<PluginRoute['accessMode']>('requests')
const personIds = ref('')
const hourly = ref(20)
const personalHourly = ref(5)
const eligibility = ref<RouteEligibility[]>([])
const eligibilityReady = ref(false)
const eligibilityError = ref('')
const projectOptions = computed(() => [
  { value: '', label: 'No project' },
  ...state.projects.map(item => ({ value: item.id, label: item.name })),
])
const participationOptions = [
  { value: 'allowlist', label: 'Listed people only' },
  { value: 'requests', label: 'Approve people as they ask' },
  { value: 'everyone', label: 'Anyone' },
]
const agentOptions = computed(() => {
  const results = new Map(eligibility.value.map(item => [item.agentId, item]))
  return state.agents.map((item) => {
    const result = results.get(item.id)
    return {
      value: item.id,
      label: item.name,
      description: result?.reason,
      disabled: !result?.eligible,
    }
  })
})
let generation = 0
let previewGeneration = 0
let reader: AbortController | undefined
let previewReader: AbortController | undefined

const userIds = computed(() => [...new Set(personIds.value.split('\n').map(value => value.trim()).filter(Boolean))])
const conditionsValid = computed(() => conditions.value.length
  ? conditions.value.length <= 8 && new Set(conditions.value.map(item => item.key)).size === conditions.value.length
  && conditions.value.every(item => /^\w{1,40}$/.test(item.key) && item.value.length > 0 && new TextEncoder().encode(item.value).length <= 200 && !/\p{Cc}/u.test(item.value))
  : catchAll.value)
const canSave = computed(() => props.enabled && ready.value && !busy.value && eligibilityReady.value
  && Boolean(name.value.trim()) && Array.from(name.value.trim()).length <= 100 && !/\p{Cc}/u.test(name.value.trim())
  && eligibility.value.some(item => item.agentId === agent.value && item.eligible)
  && conditionsValid.value && (mode.value !== 'allowlist' || userIds.value.length > 0)
  && userIds.value.length <= 100 && userIds.value.every(value => /^[\w.:-]{1,64}$/.test(value))
  && Number.isInteger(hourly.value) && hourly.value >= 1 && hourly.value <= 100
  && Number.isInteger(personalHourly.value) && personalHourly.value >= 1 && personalHourly.value <= 50)

async function load(current: number) {
  ready.value = false
  const controller = new AbortController()
  reader = controller
  try {
    const result = await pluginManagementItems<PluginRoute>(`${props.path}/routes`, controller.signal)
    if (current === generation) {
      routes.value = result
      ready.value = true
    }
  }
  finally {
    if (reader === controller)
      reader = undefined
  }
}

async function refresh() {
  if (busy.value || editing.value || removing.value)
    return
  people.value = undefined
  busy.value = true
  error.value = ''
  const current = ++generation
  try {
    await load(current)
  }
  catch (e) {
    if (current === generation)
      error.value = (e as Error).message
  }
  finally {
    if (current === generation)
      busy.value = false
  }
}

function close() {
  if (writing.value)
    return
  generation++
  previewGeneration++
  reader?.abort()
  previewReader?.abort()
  reader = undefined
  previewReader = undefined
  opened.value = false
  busy.value = false
  ready.value = false
  editing.value = undefined
  removing.value = undefined
  people.value = undefined
  emit('activity', false)
}

function edit(route?: PluginRoute) {
  if (!props.enabled || !ready.value || busy.value)
    return
  name.value = route?.name ?? ''
  enabled.value = route?.enabled ?? true
  conditions.value = Object.entries(route?.match ?? {}).map(([key, value]) => ({ key, value }))
  catchAll.value = Boolean(route && !conditions.value.length)
  agent.value = route?.agentId ?? ''
  project.value = route?.projectId ?? ''
  mode.value = route?.accessMode ?? 'requests'
  personIds.value = (route?.userIds ?? []).join('\n')
  hourly.value = route?.maxMessagesPerHour ?? 20
  personalHourly.value = route?.maxPerPersonPerHour ?? 5
  people.value = undefined
  editing.value = { route }
}

watch(() => [Boolean(editing.value), project.value, mode.value], async () => {
  const current = ++previewGeneration
  previewReader?.abort()
  eligibilityReady.value = false
  eligibility.value = []
  eligibilityError.value = ''
  if (!editing.value)
    return
  const controller = new AbortController()
  previewReader = controller
  const query = new URLSearchParams({ accessMode: mode.value })
  if (project.value)
    query.set('projectId', project.value)
  try {
    const result = await api<{ items: RouteEligibility[] }>(`${props.path}/route-eligibility?${query}`, { signal: controller.signal })
    if (current === previewGeneration) {
      eligibility.value = result.items
      eligibilityReady.value = true
    }
  }
  catch (e) {
    if (current === previewGeneration)
      eligibilityError.value = (e as Error).message
  }
  finally {
    if (previewReader === controller)
      previewReader = undefined
  }
}, { flush: 'sync' })

async function mutate(target: string, method: 'POST' | 'PUT' | 'DELETE', body: unknown) {
  if (busy.value || !ready.value || !props.enabled)
    return
  busy.value = true
  writing.value = true
  ready.value = false
  error.value = ''
  const current = ++generation
  try {
    await api(target, { method, body: JSON.stringify(body) })
    if (current !== generation)
      return
    editing.value = undefined
    removing.value = undefined
    people.value = undefined
    writing.value = false
    await load(current)
  }
  catch (e) {
    if (current === generation) {
      editing.value = undefined
      removing.value = undefined
      people.value = undefined
      error.value = `${(e as Error).message} Refresh routes before making another change.`
    }
  }
  finally {
    if (current === generation) {
      writing.value = false
      busy.value = false
    }
  }
}

async function save() {
  if (!canSave.value || !editing.value)
    return
  const route = editing.value.route
  await mutate(`${props.path}/routes${route ? `/${route.id}` : ''}`, route ? 'PUT' : 'POST', {
    name: name.value.trim(),
    enabled: enabled.value,
    match: Object.fromEntries(conditions.value.map(item => [item.key, item.value])),
    agentId: agent.value,
    projectId: project.value || null,
    accessMode: mode.value,
    userIds: userIds.value,
    people: (route?.people ?? []).filter(item => userIds.value.includes(item.id)),
    maxMessagesPerHour: hourly.value,
    maxPerPersonPerHour: personalHourly.value,
    ...(route && { revision: route.revision }),
  })
}

async function showPeople(route: PluginRoute) {
  if (busy.value || !ready.value)
    return
  busy.value = true
  error.value = ''
  const current = ++generation
  const controller = new AbortController()
  reader = controller
  try {
    const result = await api<{ items: (AccessRequest & { status: string })[] }>(`${props.path}/routes/${route.id}/requests`, { signal: controller.signal })
    if (current === generation)
      people.value = { route, requests: result.items.filter(item => item.status === 'pending') }
  }
  catch (e) {
    if (current === generation)
      error.value = (e as Error).message
  }
  finally {
    if (reader === controller)
      reader = undefined
    if (current === generation)
      busy.value = false
  }
}

function personAction(route: PluginRoute, person: string, action: 'approve' | 'deny' | 'remove') {
  return mutate(`${props.path}/routes/${route.id}/people/${encodeURIComponent(person)}`, 'POST', { action, revision: route.revision })
}

onBeforeUnmount(() => {
  generation++
  previewGeneration++
  reader?.abort()
  previewReader?.abort()
  emit('activity', false)
})
</script>

<template>
  <UiButton size="small" :disabled="!props.enabled" @click="opened = true; emit('activity', true); refresh()">
    Conversation routes
  </UiButton>
  <Modal v-if="opened" :title="`Conversation routes · ${props.name}`" @close="close">
    <div class="grid min-w-0 gap-4 p-6">
      <p class="text-sm text-muted">
        Choose who answers each source and who may participate. A more specific route takes priority; a paused route blocks its matching sources. Equally specific matches require review.
      </p>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <UiButton size="small" :disabled="busy || Boolean(editing || removing)" @click="refresh">
        {{ busy ? 'Loading routes…' : 'Refresh routes' }}
      </UiButton>
      <template v-if="ready && !editing && !people">
        <p v-if="!routes.length" class="text-sm text-muted">
          No routes yet. No one can start a conversation through this resource.
        </p>
        <article
          v-for="route in routes"
          :key="route.id"
          class="grid min-w-0 gap-2 rounded-xl border border-line p-3"
          :aria-label="route.name"
        >
          <b class="break-words text-sm">{{ route.name }} · {{ route.enabled ? 'Enabled' : 'Paused' }}</b>
          <p class="break-words text-xs">
            {{ state.agents.find(item => item.id === route.agentId)?.name ?? 'Removed agent' }} · {{ route.projectId ? state.projects.find(item => item.id === route.projectId)?.name ?? 'Removed project' : 'No project' }}
          </p>
          <p v-for="(value, key) in route.match" :key="key" class="break-words text-xs">
            {{ key }} = {{ value }}
          </p>
          <p v-if="!Object.keys(route.match).length" class="text-xs">
            All sources in this resource
          </p>
          <p class="text-xs">
            {{ route.maxMessagesPerHour }} messages per hour · {{ route.accessMode === 'everyone' ? `${route.maxPerPersonPerHour} per person · anyone` : `${route.userIds.length} people · ${route.accessMode === 'requests' ? 'approve as they ask' : 'listed people only'}` }}
          </p>
          <div class="flex flex-wrap gap-2">
            <UiButton size="small" :disabled="busy || Boolean(removing) || !props.enabled" @click="edit(route)">
              Edit route
            </UiButton>
            <UiButton size="small" :disabled="busy || Boolean(removing) || !props.enabled" @click="showPeople(route)">
              People and requests
            </UiButton>
            <UiButton size="small" :disabled="busy || Boolean(removing) || !props.enabled" @click="removing = route">
              Delete route
            </UiButton>
          </div>
          <div v-if="removing?.id === route.id" class="grid gap-2" role="alert">
            <p class="text-xs">
              Delete {{ route.name }}? A broader route may then match its sources.
            </p>
            <div class="flex gap-2">
              <UiButton size="small" :disabled="busy" @click="removing = undefined">
                Cancel delete
              </UiButton>
              <UiButton size="small" :disabled="busy || !props.enabled" @click="mutate(`${path}/routes/${route.id}`, 'DELETE', { revision: route.revision })">
                Confirm delete
              </UiButton>
            </div>
          </div>
        </article>
        <UiButton :disabled="busy || Boolean(removing) || !props.enabled || routes.length >= 256" @click="edit()">
          Add route
        </UiButton>
      </template>
      <section v-if="ready && people" class="grid gap-3" :aria-label="`People · ${people.route.name}`">
        <b>{{ people.route.name }}</b>
        <p v-if="people.route.accessMode === 'everyone'" class="text-sm">
          Anyone access does not use a people list. Edit this route and choose a restricted access mode to remove individual participants.
        </p>
        <p v-if="!people.route.userIds.length" class="text-sm text-muted">
          No listed people.
        </p>
        <div v-for="person in people.route.userIds" :key="person" class="flex items-center justify-between gap-2">
          <span class="break-words text-sm">{{ people.route.people.find(item => item.id === person)?.name || person }}</span>
          <UiButton size="small" :disabled="busy || !props.enabled || people.route.accessMode === 'everyone'" @click="personAction(people.route, person, 'remove')">
            Remove {{ person }}
          </UiButton>
        </div>
        <h3 class="text-sm! font-bold">
          Pending requests
        </h3>
        <p v-if="!people.requests.length" class="text-sm text-muted">
          No pending requests.
        </p>
        <div v-for="request in people.requests" :key="request.id" class="grid gap-2 rounded-lg border border-line p-3">
          <span class="break-words text-sm">{{ request.name || request.userId }}</span>
          <div class="flex gap-2">
            <UiButton size="small" :disabled="busy || !props.enabled || people.route.accessMode === 'everyone'" @click="personAction(people.route, request.userId, 'approve')">
              Approve {{ request.userId }}
            </UiButton>
            <UiButton size="small" :disabled="busy || !props.enabled || people.route.accessMode === 'everyone'" @click="personAction(people.route, request.userId, 'deny')">
              Deny {{ request.userId }}
            </UiButton>
          </div>
        </div>
        <UiButton :disabled="busy" @click="people = undefined">
          Back to routes
        </UiButton>
      </section>
      <form
        v-if="ready && editing"
        class="grid min-w-0 gap-4"
        :aria-label="editing.route ? 'Edit route' : 'New route'"
        @submit.prevent="save"
      >
        <div class="grid gap-1">
          <label>Route name<input
            v-model="name"
            required
            maxlength="200"
            :aria-describedby="`${formId}-name-help`"
            :disabled="busy || !props.enabled"
          ></label>
          <p :id="`${formId}-name-help`" class="text-xs text-muted">
            Up to 100 characters.
          </p>
        </div>
        <label class="flex flex-row! gap-2!"><input
          v-model="enabled"
          type="checkbox"
          class="w-auto!"
          :disabled="busy || !props.enabled"
        >Enabled</label>
        <VirtualSelect
          v-model="project"
          label="Project"
          :options="projectOptions"
          :disabled="busy || !props.enabled"
        />
        <VirtualSelect
          v-model="mode"
          label="Who may participate"
          :options="participationOptions"
          :disabled="busy || !props.enabled"
        />
        <UiAlert v-if="eligibilityError">
          {{ eligibilityError }}
        </UiAlert>
        <p v-if="!eligibilityReady && !eligibilityError" class="text-xs text-muted">
          Checking eligible agents…
        </p>
        <VirtualSelect
          v-model="agent"
          label="Agent"
          :options="agentOptions"
          placeholder="Choose an agent"
          required
          :loading="!eligibilityReady && !eligibilityError"
          :disabled="busy || !eligibilityReady || !props.enabled"
        />
        <p class="text-xs text-muted">
          The agent must be dedicated to the selected project. Anyone access requires a read-only agent without installation, GitHub, MCP or plugin access.
        </p>
        <fieldset class="grid min-w-0 gap-2">
          <legend class="text-sm font-bold">
            Source conditions
          </legend>
          <p class="text-xs text-muted">
            Advanced: exact source-context keys and values provided by the integration. All conditions must match.
          </p>
          <div v-for="(condition, index) in conditions" :key="index" class="grid min-w-0 gap-2 rounded-lg border border-line p-3">
            <label>Condition {{ index + 1 }} key<input
              v-model="condition.key"
              maxlength="40"
              required
              :disabled="busy || !props.enabled"
            ></label>
            <label>Condition {{ index + 1 }} value<input
              v-model="condition.value"
              maxlength="200"
              required
              :disabled="busy || !props.enabled"
            ></label>
            <UiButton
              type="button"
              size="small"
              :disabled="busy || !props.enabled"
              @click="conditions.splice(index, 1); catchAll = false"
            >
              Remove condition {{ index + 1 }}
            </UiButton>
          </div>
          <UiButton
            type="button"
            size="small"
            :disabled="busy || conditions.length >= 8 || !props.enabled"
            @click="conditions.push({ key: '', value: '' }); catchAll = false"
          >
            Add condition
          </UiButton>
          <label v-if="!conditions.length" class="flex flex-row! items-center gap-2!"><input
            v-model="catchAll"
            type="checkbox"
            class="w-auto!"
            :disabled="busy || !props.enabled"
          >Match all sources in this resource</label>
        </fieldset>
        <div class="grid gap-1">
          <label>Allowed person IDs<textarea
            v-model="personIds"
            rows="3"
            :aria-describedby="`${formId}-people-help`"
            :disabled="busy || !props.enabled"
          /></label>
          <p :id="`${formId}-people-help`" class="text-xs text-muted">
            One ID per line, up to 100. These people are allowed immediately. Requests require approval; anyone access does not use this list.
          </p>
        </div>
        <label>Messages per hour<input
          v-model.number="hourly"
          type="number"
          min="1"
          max="100"
          step="1"
          required
          :disabled="busy || !props.enabled"
        ></label>
        <div class="grid gap-1">
          <label>Messages per person per hour<input
            v-model.number="personalHourly"
            type="number"
            min="1"
            max="50"
            step="1"
            required
            :aria-describedby="`${formId}-personal-limit-help`"
            :disabled="busy || !props.enabled"
          ></label>
          <p :id="`${formId}-personal-limit-help`" class="text-xs text-muted">
            Applies to anyone access.
          </p>
        </div>
        <div class="flex gap-2">
          <UiButton type="button" :disabled="busy" @click="editing = undefined">
            Cancel edit
          </UiButton><UiButton type="submit" variant="primary" :disabled="!canSave">
            Save route
          </UiButton>
        </div>
      </form>
    </div>
  </Modal>
</template>
