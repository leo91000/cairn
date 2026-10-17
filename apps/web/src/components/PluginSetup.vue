<script setup lang="ts">
import type { FormResult, Manifest, PluginSetupDraft } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  useId,
} from 'vue'
import { api, state } from '../api'
import {
  delegates,
  eligibleAgent,
  fieldSettings,
  fieldValues,
  formResult,
  unshownFormErrors,
} from '../plugins'
import Modal from './Modal.vue'
import PluginFields from './PluginFields.vue'
import PluginResources from './PluginResources.vue'
import PluginView from './PluginView.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ plugin: string, manifest: Manifest, initial: PluginSetupDraft }>()
const emit = defineEmits<{ close: [], changed: [], activated: [] }>()
const draft = ref(props.initial)
const name = ref(props.initial.name)
const values = ref(fieldValues(props.manifest.settings ?? [], props.initial.settings))
const secrets = ref<Record<string, string>>({})
const token = ref('')
const credentialErrorId = useId()
const enabled = ref(true)
const agentId = ref('')
const agentIds = ref<string[]>([])
const completion = ref<FormResult>()
const completing = ref(false)
const busy = ref(false)
const resources = ref({ busy: false, editing: false })
const error = ref('')
const formFailure = ref('')
const formReady = ref(!props.manifest.instanceForm)
const expired = ref(props.initial.expiresAt <= Date.now())
const path = `/plugins/${props.plugin}/setup-drafts/${props.initial.id}`
let asked = 0
let timer: ReturnType<typeof setTimeout> | undefined
let expiryTimer: ReturnType<typeof setTimeout> | undefined

function settings() {
  return fieldSettings(props.manifest.settings ?? [], values.value)
}

function canonical(value: Record<string, any>) {
  return JSON.stringify(Object.entries(value).sort(([a], [b]) => a.localeCompare(b)))
}

const dirty = computed(() => name.value.trim() !== draft.value.name
  || canonical(settings()) !== canonical(draft.value.settings)
  || Boolean(token.value.trim()) || Object.values(secrets.value).some(value => value.trim()))
const available = computed(() => draft.value.status === 'setup' && !expired.value)
const eligible = computed(() => state.agents.filter(eligibleAgent))

async function complete(changed?: string) {
  if (!props.manifest.instanceForm)
    return
  const request = ++asked
  formReady.value = false
  formFailure.value = ''
  completing.value = true
  try {
    const result = formResult(await api<FormResult | null>(`${path}/form`, {
      method: 'POST',
      body: JSON.stringify({
        revision: draft.value.revision,
        settings: settings(),
        secrets: secrets.value,
        ...(token.value ? { token: token.value } : {}),
        changed,
      }),
    }))
    if (request !== asked)
      return
    for (const [key, value] of Object.entries(result.values ?? {}))
      values.value[key] = value ?? ''
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
  if (!props.manifest.instanceForm)
    return
  clearTimeout(timer)
  asked++
  formReady.value = false
  completing.value = true
  timer = setTimeout(() => void complete(changed), 350)
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

async function save() {
  if (completing.value || resources.value.busy)
    return
  await action(async () => {
    draft.value = await api<PluginSetupDraft>(path, {
      method: 'PATCH',
      body: JSON.stringify({
        revision: draft.value.revision,
        name: name.value,
        settings: settings(),
        secrets: secrets.value,
        ...(token.value ? { token: token.value } : {}),
      }),
    })
    name.value = draft.value.name
    values.value = fieldValues(props.manifest.settings ?? [], draft.value.settings)
    secrets.value = {}
    token.value = ''
    emit('changed')
    await complete()
  })
}

async function activate() {
  if (dirty.value || completing.value || resources.value.busy || resources.value.editing)
    return
  await action(async () => {
    const agent = state.agents.find(agent => agent.id === agentId.value)
    await api(`${path}/activate`, {
      method: 'POST',
      body: JSON.stringify({
        revision: draft.value.revision,
        enabled: enabled.value,
        agentIds: agentIds.value,
        ...(delegates(props.manifest) && { agentId: agentId.value, projectId: agent?.access.projects?.[0] ?? null, accessMode: 'requests' }),
      }),
    })
    emit('activated')
  })
}

onMounted(() => {
  expiryTimer = setTimeout(() => {
    expired.value = true
  }, Math.max(0, draft.value.expiresAt - Date.now()))
  void complete()
})
onBeforeUnmount(() => {
  clearTimeout(timer)
  clearTimeout(expiryTimer)
  asked++
  token.value = ''
  secrets.value = {}
})
</script>

<template>
  <Modal :title="`Set up ${manifest.name}`" wide @close="emit('close')">
    <div class="grid min-w-0 gap-5 p-6 phone:p-4">
      <p class="text-xs text-muted">
        Setup is saved for 24 hours. The instance stays inactive until you finish. Only saved changes survive closing this window.
      </p>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <UiAlert v-if="formFailure">
        {{ formFailure }}
      </UiAlert>
      <UiAlert v-if="!available">
        This setup expired or its package needs review. Close it and reload the plugin page.
      </UiAlert>
      <label>Name<input v-model="name" :disabled="busy || !available" maxlength="100"></label>
      <label v-if="manifest.credential">{{ manifest.credential.label }}<input
        v-model="token"
        type="password"
        autocomplete="new-password"
        :disabled="busy || !available"
        :aria-invalid="Boolean(completion?.errors?.credential)"
        :aria-describedby="completion?.errors?.credential ? credentialErrorId : undefined"
        :placeholder="draft.credential ? 'Saved — leave blank to keep' : ''"
        @input="typed('credential')"
      ></label>
      <p
        v-if="completion?.errors?.credential"
        :id="credentialErrorId"
        class="text-sm text-coral"
        role="alert"
      >
        {{ completion.errors.credential }}
      </p>
      <div class="grid min-w-0 gap-4" @input="typed()">
        <PluginFields
          v-model:values="values"
          v-model:secrets="secrets"
          :fields="(manifest.settings ?? []).map(field => ({ ...field, required: false }))"
          :saved="draft.secretKeys"
          :disabled="busy || !available"
          :options="completion?.options"
          :errors="completion?.errors"
          @refresh="typed"
        />
      </div>
      <PluginView v-if="completion?.view?.length" :nodes="completion.view" />
      <p v-if="completion?.errors?.form" class="text-sm text-coral" role="alert">
        {{ completion.errors.form }}
      </p>
      <p
        v-for="(message, index) in unshownFormErrors(manifest, completion?.errors)"
        :key="index"
        class="text-sm text-coral"
        role="alert"
      >
        {{ message }}
      </p>
      <div class="flex flex-wrap items-center gap-3">
        <UiButton :disabled="busy || completing || resources.busy || !name.trim() || !available" @click="save">
          Save setup
        </UiButton>
        <span class="text-xs text-muted" role="status">{{ completing ? 'Checking settings…' : dirty ? 'Unsaved changes' : 'All changes saved' }}</span>
      </div>
      <p v-if="dirty" class="text-xs text-muted">
        Save your changes before configuring resources or activating the instance.
      </p>
      <PluginResources
        v-if="manifest.resources?.length"
        :plugin="plugin"
        :manifest="manifest"
        :instance="draft.id"
        :setup-revision="draft.revision"
        :disabled="busy || dirty || completing || !available"
        @state="resources = $event"
      />
      <section v-if="manifest.agentAccess" class="grid gap-2" aria-label="Instance agent access">
        <h3 class="text-sm! font-bold">
          Instance commands and tools
        </h3>
        <p class="text-xs text-muted">
          No agent has access by default. Resource permissions are configured separately after activation.
        </p>
        <label v-for="agent in state.agents" :key="agent.id" class="flex flex-row! items-center gap-2!">
          <input
            v-model="agentIds"
            type="checkbox"
            class="w-auto!"
            :value="agent.id"
            :disabled="busy || !available"
          >{{ agent.name }}
        </label>
      </section>
      <label v-if="delegates(manifest)">Agent for incoming conversations<select v-model="agentId" :disabled="busy || !available">
        <option value="">Choose a dedicated agent</option>
        <option v-for="agent in eligible" :key="agent.id" :value="agent.id">{{ agent.name }}</option>
      </select><small>People must be approved before their messages are accepted. You can configure people and limits after activation.</small></label>
      <label class="flex flex-row! items-center gap-2!"><input
        v-model="enabled"
        type="checkbox"
        class="w-auto!"
        :disabled="busy || !available"
      >Start immediately</label>
      <div class="flex flex-wrap justify-end gap-2 border-t border-line pt-4">
        <UiButton :disabled="busy || dirty || completing || resources.busy || resources.editing || !available || !formReady || Object.keys(completion?.errors ?? {}).length > 0 || (delegates(manifest) && !agentId)" variant="primary" @click="activate">
          Activate instance
        </UiButton>
      </div>
    </div>
  </Modal>
</template>
