<script setup lang="ts">
import type { FormResult, Manifest, PluginSetupDraft } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  onMounted,
  ref,
  useId,
  watch,
} from 'vue'
import { api, state } from '../api'
import {
  credentialInput,
  signInProblem,
  useDeviceSignIn,
  verificationLink,
} from '../device-sign-in'
import { pluginFormLookup } from '../plugin-form-lookups'
import {
  pluginFormSecrets,
  pluginFormValue,
  pluginFormValues,
  pluginSecrets,
  pluginSettings,
} from '../plugin-form-values'
import { pluginOperation } from '../plugin-operations'
import {
  delegates,
  delegationRisks,
  formResult,
  listed,
  unshownFormErrors,
} from '../plugins'
import Modal from './Modal.vue'
import PluginFields from './PluginFields.vue'
import PluginResources from './PluginResources.vue'
import PluginView from './PluginView.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'
import UiNotice from './UiNotice.vue'
import VirtualSelect from './VirtualSelect.vue'

const props = defineProps<{ plugin: string, manifest: Manifest, initial: PluginSetupDraft }>()
const emit = defineEmits<{ close: [], changed: [], activated: [] }>()
const draft = ref(props.initial)
const name = ref(props.initial.name)
const values = ref(pluginFormValues(props.manifest.settings ?? [], props.initial.settings))
const secrets = ref<Record<string, string>>(pluginFormSecrets())
const token = ref('')
const credentialErrorId = useId()
const enabled = ref(true)
const agentId = ref('')
const completion = ref<FormResult>()
const completing = ref(false)
const busy = ref(false)
const resources = ref({ busy: false, editing: false, pending: false })
const resourceWork = computed(() => resources.value.busy || resources.value.editing || resources.value.pending)
const error = ref('')
const formFailure = ref('')
const formReady = ref(!props.manifest.instanceForm)
const expired = ref(props.initial.expiresAt <= Date.now())
const path = `/plugins/${props.plugin}/setup-drafts/${props.initial.id}`
const oauth = computed(() => props.manifest.credential?.oauth)
const signIn = useDeviceSignIn(() => props.plugin, () => {}, 2000, {
  kind: () => 'oauth',
  input: () => ({ revision: draft.value.revision }),
  startPath: () => `${path}/oauth`,
})
const signingIn = computed(() => signIn.starting.value || signIn.pending.value)
let asked = 0
let timer: ReturnType<typeof setTimeout> | undefined
let expiryTimer: ReturnType<typeof setTimeout> | undefined

function settings() {
  return pluginSettings(props.manifest.settings ?? [], values.value)
}

function canonical(value: Record<string, any>) {
  return JSON.stringify(Object.entries(value).sort(([a], [b]) => a.localeCompare(b)))
}

const edited = computed(() => name.value.trim() !== draft.value.name
  || canonical(settings()) !== canonical(draft.value.settings)
  || Boolean(token.value.trim()) || Object.keys(pluginSecrets(secrets.value)).length > 0)
const dirty = computed(() => edited.value || signIn.completed.value)
const available = computed(() => draft.value.status === 'setup' && !expired.value)
const chosenAgent = computed(() => state.agents.find(agent => agent.id === agentId.value))
const chosenRisks = computed(() => chosenAgent.value ? delegationRisks(chosenAgent.value, [], false) : [])
const consentBasis = computed(() => {
  if (!oauth.value)
    return ''
  const keys = [oauth.value.clientIdSetting, ...Object.keys(oauth.value.scopeSettings ?? {})]
  const secret = oauth.value.clientSecretSetting
  return JSON.stringify([draft.value.revision, keys.map(key => values.value[key] ?? null), secret ? secrets.value[secret] ?? '' : ''])
})
watch(consentBasis, () => {
  if (signIn.flow.value || signIn.starting.value)
    void signIn.cancel()
})
watch(available, (value) => {
  if (!value)
    void signIn.cancel()
})

function formBody() {
  return {
    revision: draft.value.revision,
    settings: settings(),
    secrets: secrets.value,
    ...(token.value ? { token: token.value } : {}),
  }
}

const lookup = pluginFormLookup(() => `${path}/form`, formBody, () => props.manifest.version)

async function complete(changed?: string) {
  if (!props.manifest.instanceForm)
    return
  const request = ++asked
  formReady.value = false
  formFailure.value = ''
  completing.value = true
  try {
    const result = formResult(await pluginOperation<FormResult | null>(`${path}/form`, {
      ...formBody(),
      changed,
    }))
    if (request !== asked)
      return
    for (const field of props.manifest.settings ?? []) {
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
  if (completing.value || resourceWork.value || signingIn.value)
    return
  await action(async () => {
    const saved = await api<PluginSetupDraft>(path, {
      method: 'PATCH',
      body: JSON.stringify({
        revision: draft.value.revision,
        name: name.value,
        settings: settings(),
        secrets: pluginSecrets(secrets.value),
        ...credentialInput(token.value, signIn.flow.value),
      }),
    })
    signIn.consumed()
    draft.value = saved
    name.value = draft.value.name
    values.value = pluginFormValues(props.manifest.settings ?? [], draft.value.settings)
    secrets.value = pluginFormSecrets()
    token.value = ''
    emit('changed')
    await complete()
  })
}

async function activate() {
  if (dirty.value || completing.value || resourceWork.value || signingIn.value)
    return
  await action(async () => {
    const agent = state.agents.find(agent => agent.id === agentId.value)
    await pluginOperation(`${path}/activate`, {
      revision: draft.value.revision,
      enabled: enabled.value,
      agentIds: [],
      ...(delegates(props.manifest) && { agentId: agentId.value, projectId: agent?.access.projects?.[0] ?? null, accessMode: 'requests' }),
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
  void signIn.cancel()
  token.value = ''
  secrets.value = pluginFormSecrets()
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
      <label>Name<input v-model="name" :disabled="busy || resourceWork || signingIn || !available" maxlength="100"></label>
      <section v-if="oauth" class="grid gap-3" :aria-label="`${manifest.name} sign-in`">
        <p class="text-sm font-semibold">
          {{ manifest.credential?.label }}
        </p>
        <p class="text-xs text-muted">
          Save your app settings before signing in. The credential stays on the installation.
        </p>
        <UiAlert v-if="signIn.error.value || signInProblem(signIn.flow.value)">
          {{ signIn.error.value || signInProblem(signIn.flow.value) }}
        </UiAlert>
        <div class="flex flex-wrap items-center gap-3">
          <UiButton
            :disabled="busy || resourceWork || completing || !available || edited || signingIn || signIn.completed.value"
            @click="signIn.start"
          >
            Sign in with {{ manifest.name }}
          </UiButton>
          <template v-if="signIn.pending.value && signIn.flow.value">
            <a :href="verificationLink(signIn.flow.value)" target="_blank" rel="noopener noreferrer">Open sign-in page</a>
          </template>
          <UiButton v-if="signIn.flow.value || signIn.starting.value" @click="signIn.cancel">
            Cancel sign-in
          </UiButton>
        </div>
        <p class="text-xs text-muted" role="status">
          {{ signIn.completed.value ? 'Sign-in complete. Save setup to keep this account.' : signingIn ? 'Waiting for sign-in…' : draft.credential ? 'An account is saved.' : 'No account connected yet.' }}
        </p>
      </section>
      <label v-if="manifest.credential && !oauth">{{ manifest.credential.label }}<input
        v-model="token"
        type="password"
        autocomplete="new-password"
        :disabled="busy || resourceWork || !available"
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
          :lookup="lookup"
          :saved="draft.secretKeys"
          :disabled="busy || resourceWork || signingIn || !available"
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
        <UiButton :disabled="busy || completing || resourceWork || signingIn || !name.trim() || !available" @click="save">
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
        :disabled="busy || dirty || signingIn || completing || !available"
        @state="resources = $event"
      />
      <section v-if="manifest.agentAccess" class="grid gap-2" aria-label="Instance agent access">
        <h3 class="text-sm! font-bold">
          Instance commands and tools
        </h3>
        <p class="text-xs text-muted">
          After activation, grant instance access from each agent’s page. Resource permissions are configured separately on each resource.
        </p>
      </section>
      <div v-if="delegates(manifest)" class="grid gap-1">
        <VirtualSelect
          v-model="agentId"
          label="Agent for incoming conversations"
          placeholder="Choose an agent"
          :options="state.agents.map(agent => ({ value: agent.id, label: agent.name }))"
          :disabled="busy || resourceWork || !available"
          required
        />
        <small>People must be approved before their messages are accepted. You can configure people and limits after activation.</small>
        <UiNotice
          v-if="chosenAgent && chosenRisks.length"
          tone="warning"
          role="status"
          :title="`The people you allow can use everything ${chosenAgent.name} can do`"
        >
          Through the bot, they can {{ listed(chosenRisks) }}. Only allow people you trust, or choose a more limited agent.
        </UiNotice>
      </div>
      <label class="flex flex-row! items-center gap-2!"><input
        v-model="enabled"
        type="checkbox"
        class="w-auto!"
        :disabled="busy || resourceWork || !available"
      >Start immediately</label>
      <div class="flex flex-wrap justify-end gap-2 border-t border-line pt-4">
        <UiButton :disabled="busy || dirty || signingIn || completing || resourceWork || !available || !formReady || Object.keys(completion?.errors ?? {}).length > 0 || (delegates(manifest) && !agentId)" variant="primary" @click="activate">
          Activate instance
        </UiButton>
      </div>
    </div>
  </Modal>
</template>
