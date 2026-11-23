<script setup lang="ts">
import type { Account, ApiKeyProvider, Provider } from '../../../../packages/contracts/accounts'
import type { AccountsState } from '../accounts'
import { computed, ref, watch } from 'vue'
import { providers } from '../../../../packages/contracts/accounts'
import { listCodingAgentAccountProviders } from '../../../../packages/contracts/generated/listCodingAgentAccountProviders'
import {
  antigravityTermsWarning,
  brands,
  connectsWithKey,
  credentialsProject,
  googleAccount,
  googleCloudKey,
} from '../accounts'
import { api, notify } from '../api'
import {
  ArrowUpRight,
  Check,
  Copy,
  LoaderCircle,
} from '../icons'
import { buttonBase, buttonSizes, buttonVariants } from '../ui'
import Icon from './Icon.vue'
import Modal from './Modal.vue'
import ProviderMark from './ProviderMark.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

// Adds an account or signs one in again, with the same steps for every coding agent: the
// beacon page opens in a new tab and this window follows until the account is verified.
const props = defineProps<{ accounts: AccountsState, provider?: Provider, account?: Account }>()
const emit = defineEmits<{ close: [] }>()
const chosen = ref<Provider>(props.provider ?? 'codex')
const name = ref(props.account?.name ?? '')
const apiKey = ref('')
// An Antigravity account whose sign-in never finished reconnects by signing in.
const modelProvider = ref(props.account?.modelProvider ?? (props.account?.provider === 'antigravity' ? googleAccount : 'opencode'))
const keyProviders = ref<ApiKeyProvider[]>([])
const providerError = ref('')
const loadingProviders = ref(false)
// Antigravity offers a Gemini API key, Google Cloud credentials or a Google account sign-in.
const offersChoice = computed(() => providers[chosen.value].connection === 'choice')
const choices = computed(() => offersChoice.value
  ? [
      ...keyProviders.value.map(item => ({
        id: item.id,
        name: item.id === 'google-cloud' ? 'Google Cloud credentials' : item.id === 'gemini' ? 'Gemini API key' : item.name,
        description: item.id === 'google-cloud' ? 'Service account or authorized-user JSON, with a project and region.' : 'Recommended. A key from Google AI Studio.',
      })),
      { id: googleAccount, name: 'Google account', description: 'Sign in on Google’s page. Not allowed by Google’s terms for third-party apps.' },
    ]
  : [])
const usesKey = computed(() => connectsWithKey(chosen.value, offersChoice.value ? modelProvider.value : null))
const usesCredentials = computed(() => offersChoice.value && modelProvider.value === 'google-cloud')
const confirmsTerms = computed(() => chosen.value === 'antigravity' && modelProvider.value === googleAccount)
const acceptedTerms = ref(false)
const credentials = ref('')
const project = ref('')
const location = ref('')
const credentialsError = ref('')
const detectedProject = computed(() => credentialsProject(credentials.value))
const keyReady = computed(() => usesCredentials.value ? !!credentials.value.trim() : !!apiKey.value.trim())
const code = ref('')
const submitted = ref(false)
const copied = ref(false)
const copyError = ref('')
const flow = computed(() => props.accounts.signIn.value?.state === 'pending' || props.accounts.signIn.value?.state === 'failed' ? props.accounts.signIn.value : null)
const busy = computed(() => props.accounts.busy.value)
const title = computed(() => {
  if (!flow.value) {
    if (!props.account)
      return 'Add an account'
    return confirmsTerms.value ? 'Reconnect Google account' : usesCredentials.value ? 'Replace credentials' : 'Replace API key'
  }

  if (flow.value.state === 'failed')
    return 'Let’s try that again'
  return `Connect your ${flow.value.provider === 'antigravity' ? 'Google account' : providers[flow.value.provider].subscription}`
})
// The vendor whose page signs in and shows the authorization code to paste.
const codeVendor = computed(() => flow.value?.provider === 'antigravity' ? { page: 'Google', name: 'Google' } : { page: 'Anthropic', name: 'Claude' })
watch(chosen, async (value, _previous, onCleanup) => {
  apiKey.value = ''
  providerError.value = ''
  keyProviders.value = []
  if (providers[value].connection === 'oauth')
    return
  let current = true
  onCleanup(() => {
    current = false
  })
  loadingProviders.value = true
  try {
    const result = await listCodingAgentAccountProviders(api, { provider: value })
    if (result.kind === 'unsupported')
      throw new Error(result.message)
    const values = result.providers
    if (current) {
      keyProviders.value = values
      const signsIn = providers[value].connection === 'choice' && modelProvider.value === googleAccount
      if (!signsIn && !values.some(item => item.id === modelProvider.value))
        modelProvider.value = values[0]?.id ?? ''
    }
  }
  catch (error) {
    if (current)
      providerError.value = (error as Error).message
  }
  finally {
    if (current)
      loadingProviders.value = false
  }
}, { immediate: true })
watch(modelProvider, () => {
  apiKey.value = ''
  credentials.value = ''
  project.value = ''
  location.value = ''
  credentialsError.value = ''
  acceptedTerms.value = false
})
watch(() => props.accounts.signIn.value?.state, (state) => {
  if (state === 'complete')
    emit('close')
  // Each Google sign-in, including a retry, needs its own confirmation.
  if (state === 'failed')
    acceptedTerms.value = false
})
// Separate sources: each poll brings a new sign-in object, which must not reset the feedback.
watch([() => flow.value?.code, () => flow.value?.state], () => {
  copied.value = false
  copyError.value = ''
  submitted.value = false
})

async function readCredentials(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0]
  if (!file)
    return
  credentials.value = await file.text()
  credentialsError.value = ''
}

async function add() {
  const accountName = name.value.trim() || `${providers[chosen.value].label} account`
  if (confirmsTerms.value && !acceptedTerms.value)
    return
  if (usesKey.value) {
    let key = apiKey.value
    if (usesCredentials.value) {
      try {
        key = googleCloudKey({ credentials: credentials.value, project: project.value, location: location.value })
      }
      catch (error) {
        credentialsError.value = (error as Error).message
        return
      }
    }

    const saved = await props.accounts.connectKey(chosen.value, accountName, modelProvider.value, key, props.account?.id)
    if (saved) {
      apiKey.value = ''
      credentials.value = ''
      notify(`${saved.name} ${usesCredentials.value ? 'credentials' : 'API key'} saved`)
      emit('close')
    }
  }
  else if (props.account) {
    await props.accounts.reconnect(props.account.id)
  }
  else {
    await props.accounts.add(chosen.value, accountName)
  }
}

async function copyCode() {
  const value = flow.value?.code
  if (!value)
    return
  try {
    // Called directly from the click, without delaying the link's navigation.
    await navigator.clipboard.writeText(value)
    copied.value = true
    copyError.value = ''
  }
  catch {
    copied.value = false
    copyError.value = 'Couldn’t copy automatically. Select the code and copy it manually.'
  }
}

async function submit() {
  if (!code.value.trim())
    return
  const succeeded = await props.accounts.submitCode(code.value.trim())
  code.value = ''
  submitted.value = !!succeeded
}

async function close() {
  // Leaving before the account is verified cancels the sign-in; an account that never
  // connected is removed.
  if (flow.value)
    await props.accounts.cancel()
  emit('close')
}
</script>

<template>
  <Modal :title="title" sheet @close="close">
    <section class="grid gap-5 p-6 phone:px-5" :aria-label="title">
      <UiAlert v-if="accounts.error.value">
        {{ accounts.error.value }}
      </UiAlert>

      <form v-if="!flow" class="grid gap-5" @submit.prevent="add">
        <div
          v-if="!account"
          class="grid grid-cols-2 gap-2.5 phone:grid-cols-1"
          role="radiogroup"
          aria-label="Coding agent"
        >
          <button
            v-for="(item, key) in providers"
            :key="key"
            type="button"
            role="radio"
            :aria-checked="chosen === key"
            class="flex items-center gap-3 rounded-2xl border px-3 py-3 text-left transition-[border-color,background,box-shadow] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent"
            :class="chosen === key ? brands[key].chosen : 'border-line hover:bg-hover/40'"
            @click="chosen = key"
          >
            <ProviderMark :provider="key" :size="32" />
            <span class="min-w-0">
              <span class="block text-sm leading-tight font-semibold">{{ item.label }}</span>
              <span class="text-2xs text-muted">{{ item.subscription }}</span>
            </span>
          </button>
        </div>
        <label class="gap-1.5 text-sm text-ink">
          Name
          <input
            v-model="name"
            maxlength="100"
            autocomplete="off"
            placeholder="e.g. Work"
            :disabled="busy || !!account"
          >
        </label>
        <template v-if="offersChoice">
          <UiAlert v-if="providerError">
            {{ providerError }}
          </UiAlert>
          <div class="grid gap-2" role="radiogroup" aria-label="Connection">
            <button
              v-for="item in choices"
              :key="item.id"
              type="button"
              role="radio"
              :aria-checked="modelProvider === item.id"
              class="flex flex-col rounded-xl border px-3.5 py-2.5 text-left transition-[border-color,background] focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:opacity-50"
              :class="modelProvider === item.id ? 'border-accent bg-soft' : 'border-line hover:bg-hover/40'"
              :disabled="busy || loadingProviders || (!!account && modelProvider !== item.id)"
              @click="modelProvider = item.id"
            >
              <span class="text-sm font-semibold">{{ item.name }}</span>
              <span class="text-2xs text-muted">{{ item.description }}</span>
            </button>
          </div>
        </template>
        <template v-if="usesCredentials">
          <label class="gap-1.5 text-sm text-ink">
            Credentials JSON
            <textarea
              v-model="credentials"
              rows="5"
              spellcheck="false"
              autocomplete="off"
              required
              class="font-mono text-xs"
              placeholder="Paste a service-account or authorized-user JSON file"
              :disabled="busy || loadingProviders"
              @input="credentialsError = ''"
            />
          </label>
          <label class="gap-1.5 text-sm text-ink">
            Or choose the file
            <input
              type="file"
              accept="application/json,.json"
              :disabled="busy || loadingProviders"
              @change="readCredentials"
            >
          </label>
          <div class="grid grid-cols-2 gap-3 phone:grid-cols-1">
            <label class="gap-1.5 text-sm text-ink">
              Project
              <input
                v-model="project"
                maxlength="100"
                autocomplete="off"
                spellcheck="false"
                :placeholder="detectedProject || 'Project ID'"
                :disabled="busy || loadingProviders"
                @input="credentialsError = ''"
              >
            </label>
            <label class="gap-1.5 text-sm text-ink">
              Region
              <input
                v-model="location"
                maxlength="100"
                autocomplete="off"
                spellcheck="false"
                placeholder="global"
                :disabled="busy || loadingProviders"
              >
            </label>
          </div>
          <p v-if="credentialsError" role="alert" class="text-sm text-danger">
            {{ credentialsError }}
          </p>
          <p class="text-xs text-muted">
            The credentials are encrypted on your installation and used only for Antigravity runs. Antigravity doesn’t report usage.
          </p>
        </template>
        <template v-else-if="usesKey">
          <UiAlert v-if="providerError && !offersChoice">
            {{ providerError }}
          </UiAlert>
          <label v-if="!offersChoice" class="gap-1.5 text-sm text-ink">
            Model provider
            <select v-model="modelProvider" :disabled="busy || loadingProviders || !!account">
              <option v-for="item in keyProviders" :key="item.id" :value="item.id">{{ item.name }}</option>
            </select>
          </label>
          <label class="gap-1.5 text-sm text-ink">
            API key
            <input
              v-model="apiKey"
              type="password"
              autocomplete="new-password"
              spellcheck="false"
              maxlength="8192"
              required
              placeholder="Paste your model-provider key"
              :disabled="busy || loadingProviders"
            >
          </label>
          <p class="text-xs text-muted">
            The key is encrypted on your installation and used only for this model provider.
            {{ modelProvider === 'opencode-go' ? 'OpenCode Go usage is checked automatically.' : chosen === 'antigravity' ? 'Antigravity doesn’t report usage.' : 'Provider usage isn’t reported here.' }}
          </p>
          <p class="text-xs text-muted">
            Model access is checked when you send a message. Your provider’s plan, balance and Privacy settings still apply.
          </p>
        </template>
        <template v-else>
          <div v-if="confirmsTerms" class="grid gap-3 rounded-xl border border-warning/40 bg-warning/8 px-3.5 py-3" role="alert">
            <p class="text-sm text-ink">
              {{ antigravityTermsWarning }}
            </p>
            <label class="flex-row items-start gap-2.5 text-sm text-ink">
              <input
                v-model="acceptedTerms"
                type="checkbox"
                class="mt-0.5"
                :disabled="busy"
              >
              <span>I understand that Google may suspend this account and want to sign in anyway.</span>
            </label>
          </div>
          <p class="text-xs text-muted">
            You sign in on {{ providers[chosen].vendor }}’s page. Your password stays there.
          </p>
        </template>
        <UiButton type="submit" variant="primary" :disabled="busy || (usesKey && (loadingProviders || !keyProviders.length || !keyReady)) || (confirmsTerms && !acceptedTerms)">
          <template v-if="usesCredentials">
            {{ account ? 'Replace credentials' : 'Save credentials' }}
          </template>
          <template v-else-if="usesKey">
            {{ account ? 'Replace API key' : 'Save API key' }}
          </template>
          <template v-else>
            Continue to sign-in<Icon :name="ArrowUpRight" :size="15" />
          </template>
        </UiButton>
      </form>

      <template v-else-if="flow.state === 'failed'">
        <p role="alert" class="text-sm text-danger">
          {{ flow.error || 'Sign-in did not finish.' }}
        </p>
        <!-- Trying again starts another Google sign-in: the same warning applies. -->
        <div v-if="flow.provider === 'antigravity'" class="grid gap-3 rounded-xl border border-warning/40 bg-warning/8 px-3.5 py-3" role="alert">
          <p class="text-sm text-ink">
            {{ antigravityTermsWarning }}
          </p>
          <label class="flex-row items-start gap-2.5 text-sm text-ink">
            <input
              v-model="acceptedTerms"
              type="checkbox"
              class="mt-0.5"
              :disabled="busy"
            >
            <span>I understand that Google may suspend this account and want to sign in anyway.</span>
          </label>
        </div>
        <div class="flex flex-wrap gap-2">
          <UiButton variant="primary" :disabled="busy || (flow.provider === 'antigravity' && !acceptedTerms)" @click="accounts.reconnect(flow.accountId)">
            Try again
          </UiButton>
          <UiButton :disabled="busy" @click="close">
            Close
          </UiButton>
        </div>
      </template>

      <template v-else>
        <ol class="grid gap-1" aria-live="polite">
          <template v-if="flow.provider === 'codex'">
            <li class="flex gap-3">
              <span class="step-number" :class="flow.code ? 'done' : 'current'"><Icon v-if="flow.code" :name="Check" :size="14" /><Icon
                v-else
                :name="LoaderCircle"
                :size="14"
                class="animate-spin motion-reduce:animate-none"
              /></span>
              <p class="pt-1 text-sm font-semibold">
                {{ flow.code ? 'Code ready' : 'Getting a secure code from Codex…' }}
              </p>
            </li>
            <li class="flex gap-3">
              <span class="step-number" :class="flow.code && flow.phase !== 'verifying' ? 'current' : flow.phase === 'verifying' ? 'done' : ''">2</span>
              <div class="min-w-0 flex-1 pt-1">
                <p class="text-sm font-semibold">
                  Enter this code on OpenAI
                </p>
                <template v-if="flow.code && flow.phase !== 'verifying'">
                  <div class="mt-2.5 flex items-center justify-between gap-3 rounded-2xl border border-line bg-inset px-4 py-3">
                    <code aria-label="Verification code" class="min-w-0 select-all font-mono text-2xl font-bold tracking-widest break-all phone:text-xl">{{ flow.code }}</code>
                    <button
                      type="button"
                      class="grid size-8 shrink-0 place-items-center rounded-lg text-muted hover:bg-hover hover:text-ink"
                      :aria-label="copied ? 'Code copied' : 'Copy verification code'"
                      @click="copyCode"
                    >
                      <Icon :name="copied ? Check : Copy" :size="16" />
                    </button>
                  </div>
                  <p v-if="copyError" role="alert" class="mt-2 text-sm text-warning">
                    {{ copyError }}
                  </p>
                  <a
                    v-if="flow.url"
                    :href="flow.url"
                    target="_blank"
                    rel="noopener noreferrer"
                    class="mt-3 w-full"
                    :class="[buttonBase, buttonVariants.primary, buttonSizes.default]"
                    @click="copyCode"
                  >Copy code &amp; open sign-in<Icon :name="ArrowUpRight" :size="15" /></a>
                </template>
              </div>
            </li>
          </template>
          <template v-else>
            <li class="flex gap-3">
              <span class="step-number" :class="flow.url ? 'done' : 'current'"><Icon v-if="flow.url" :name="Check" :size="14" /><Icon
                v-else
                :name="LoaderCircle"
                :size="14"
                class="animate-spin motion-reduce:animate-none"
              /></span>
              <div class="min-w-0 flex-1 pt-1">
                <p class="text-sm font-semibold">
                  {{ flow.url ? `Sign in on ${codeVendor.page}’s page` : 'Preparing your secure sign-in link…' }}
                </p>
                <a
                  v-if="flow.url && flow.phase !== 'verifying'"
                  :href="flow.url"
                  target="_blank"
                  rel="noopener noreferrer"
                  class="mt-2.5"
                  :class="[buttonBase, buttonVariants.default, buttonSizes.small]"
                >Open {{ codeVendor.name }} sign-in<Icon :name="ArrowUpRight" :size="14" /></a>
              </div>
            </li>
            <li class="flex gap-3">
              <span class="step-number" :class="flow.acceptsCode ? 'current' : flow.phase === 'verifying' ? 'done' : ''">2</span>
              <form class="min-w-0 flex-1 pt-1" @submit.prevent="submit">
                <label class="grid gap-2 text-sm font-semibold">
                  Paste the code {{ codeVendor.page }} shows
                  <input
                    v-model="code"
                    type="password"
                    autocomplete="off"
                    spellcheck="false"
                    placeholder="Authorization code"
                    :disabled="busy || !flow.acceptsCode || submitted"
                    :aria-label="`${codeVendor.name} authorization code`"
                  >
                </label>
                <UiButton
                  v-if="flow.acceptsCode && !submitted"
                  type="submit"
                  variant="primary"
                  class="mt-3 w-full"
                  :disabled="busy || !code.trim()"
                >
                  Finish sign-in
                </UiButton>
              </form>
            </li>
          </template>
          <li class="flex gap-3">
            <span class="step-number" :class="flow.phase === 'verifying' || submitted ? 'current' : ''">3</span>
            <p class="flex items-center gap-2 pt-1 text-sm" :class="flow.phase === 'verifying' || submitted ? 'font-semibold' : 'text-muted'">
              <Icon
                v-if="flow.phase === 'verifying' || submitted"
                :name="LoaderCircle"
                :size="14"
                class="animate-spin motion-reduce:animate-none"
              />
              {{ flow.phase === 'verifying' || submitted ? 'Verifying your account…' : 'Approve access — this window finishes on its own' }}
            </p>
          </li>
        </ol>
        <UiButton size="small" :disabled="busy || flow.phase === 'verifying'" @click="close">
          Cancel sign-in
        </UiButton>
      </template>
    </section>
  </Modal>
</template>

<style scoped>
.step-number {
  display: grid;
  place-items: center;
  width: 26px;
  height: 26px;
  flex-shrink: 0;
  border-radius: 50%;
  font: 700 12px var(--font-heading);
  background: var(--color-hover);
  color: var(--color-muted);
}
.step-number.current {
  background: var(--color-accent);
  color: var(--color-on-brand);
}
.step-number.done {
  background: var(--color-soft);
  color: var(--color-accent);
}
</style>
