<script setup lang="ts">
import {
  onMounted,
  onScopeDispose,
  ref,
  watch,
} from 'vue'
import { logoutAccount, redirect, state } from './api'
import App from './App.vue'
import ThemeControl from './components/ThemeControl.vue'
import UiAlert from './components/UiAlert.vue'
import UiButton from './components/UiButton.vue'

interface AccountSession {
  authenticated: boolean
  account: { id: string, email: string } | null
  csrf: string | null
  installations: Array<{
    id: string
    name: string
    role: 'owner'
    online: boolean
  }>
}

interface SignInMethod {
  id: string
  kind: 'email' | 'google' | 'github' | 'passkey'
  label: string
}

const session = ref<AccountSession | null>(null)
const options = ref({ google: false, github: false, passkeys: false })
const methods = ref<SignInMethod[]>([])
const showMethods = ref(false)
const passkeyName = ref('My passkey')
const methodNames = {
  email: 'Email',
  google: 'Google',
  github: 'GitHub',
  passkey: 'Passkey',
}
const ready = ref(false)
const email = ref('')
const code = ref('')
const challenge = ref('')
const busy = ref(false)
const error = ref('')
const claimCode = ref('')
const editingName = ref(false)
const installationName = ref('')
const deviceCode = ref('')
const claimStatus = ref('')
const confirmDetach = ref(false)
const installation = ref<{ id: string, name: string, online: boolean } | null>(null)
const installationMenu = ref<HTMLDetailsElement>()
const officialReturnKey = 'leo-installation-return'
const claimPage = window.location.pathname === '/claim'

watch(session, (value) => {
  state.csrf = value?.csrf || ''
  state.authenticated = value?.authenticated || false
  state.ready = ready.value
  if (!value?.authenticated)
    return

  const requested = state.installationId
  installation.value = value.installations.find(item => item.id === requested) || null
  if (requested && !installation.value) {
    error.value = 'This installation is unavailable or no longer accessible to your Leo account.'
    return
  }

  if (installation.value) {
    try {
      localStorage.setItem(`leo-current-installation:${value.account?.id}`, installation.value.id)
    }
    catch {}
  }

  if (!requested && value.installations.length && !claimPage) {
    let remembered = ''
    try {
      remembered = localStorage.getItem(`leo-current-installation:${value.account?.id}`) || ''
    }
    catch {}

    openInstallation(value.installations.find(item => item.id === remembered) || value.installations[0]!)
  }
})

watch(() => state.authenticated, (authenticated) => {
  if (!authenticated && session.value?.authenticated) {
    session.value = null
    installation.value = null
    showMethods.value = false
  }
})

async function accountRequest(route: string, body?: unknown) {
  const response = await fetch(`/api/account/${route}`, {
    method: body === undefined ? 'GET' : 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.value?.csrf || '' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 204)
    return
  const text = await response.text()
  const value = text ? JSON.parse(text) : undefined
  if (!response.ok)
    throw new Error(value?.error || 'Unable to reach Leo. Please try again.')
  return value
}

async function loadSession() {
  busy.value = true
  error.value = ''
  try {
    const [account, available] = await Promise.all([accountRequest('session'), accountRequest('options')])
    session.value = account
    options.value = available
    ready.value = true
    state.ready = true
  }
  catch {
    error.value = 'Unable to reach Leo. Please try again.'
  }
  finally {
    busy.value = false
  }
}

async function submit() {
  busy.value = true
  error.value = ''
  try {
    if (challenge.value) {
      session.value = await accountRequest('verify', { challenge: challenge.value, code: code.value })
      code.value = ''
      challenge.value = ''
    }
    else {
      const value = await accountRequest('email-code', { email: email.value })
      challenge.value = value.challenge
    }
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to reach Leo. Please try again.'
  }
  finally {
    busy.value = false
  }
}

async function signOut() {
  const leavingInstallation = !!state.installationId
  busy.value = true
  error.value = ''
  try {
    await logoutAccount()
    session.value = null
    showMethods.value = false
    installation.value = null
    claimCode.value = ''
    deviceCode.value = ''
    claimStatus.value = ''
    confirmDetach.value = false
    state.installationId = ''
    email.value = ''
    code.value = ''
    challenge.value = ''
    if (leavingInstallation)
      window.location.assign('/')
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to sign out. Please try again.'
  }
  finally {
    busy.value = false
  }
}

async function openMethods() {
  busy.value = true
  error.value = ''
  try {
    methods.value = (await accountRequest('methods')).methods
    showMethods.value = true
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to load sign-in methods.'
  }
  finally {
    busy.value = false
  }
}

async function removeMethod(id: string) {
  busy.value = true
  error.value = ''
  try {
    await accountRequest('methods/remove', { id })
    methods.value = (await accountRequest('methods')).methods
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to remove sign-in method.'
  }
  finally {
    busy.value = false
  }
}

async function passkey(register: boolean) {
  busy.value = true
  error.value = ''
  try {
    if (!window.PublicKeyCredential?.parseCreationOptionsFromJSON || !PublicKeyCredential.parseRequestOptionsFromJSON)
      throw new Error('Passkeys are unavailable in this browser. Use another sign-in method.')
    const route = register ? 'register' : 'login'
    const start = await accountRequest(`passkeys/${route}/start`, {})
    const credential = register
      ? await navigator.credentials.create({ publicKey: PublicKeyCredential.parseCreationOptionsFromJSON(start.options.publicKey) })
      : await navigator.credentials.get({ publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(start.options.publicKey) })
    if (!(credential instanceof PublicKeyCredential))
      throw new Error('Passkey operation cancelled. Please try again.')
    const result = await accountRequest(`passkeys/${route}/finish`, {
      challenge: start.challenge,
      credential: credential.toJSON(),
      ...(register ? { label: passkeyName.value } : {}),
    })
    if (register) {
      methods.value = (await accountRequest('methods')).methods
      passkeyName.value = 'My passkey'
    }
    else {
      session.value = result
      challenge.value = ''
      code.value = ''
    }
  }
  catch (cause) {
    error.value = cause instanceof DOMException
      ? 'Passkey operation cancelled or unavailable. You can use another sign-in method.'
      : cause instanceof Error ? cause.message : 'Unable to use this passkey.'
  }
  finally {
    busy.value = false
  }
}

async function installationRequest(route: string, body?: unknown) {
  const response = await fetch(`/api/installations/${route}`, {
    method: 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.value?.csrf || '' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 204)
    return
  const value = await response.json()
  if (!response.ok)
    throw new Error(value.error || 'Unable to update the installation.')
  return value
}

async function approveDevice() {
  busy.value = true
  error.value = ''
  claimStatus.value = ''
  try {
    await installationRequest('device-claim', { code: deviceCode.value })
    deviceCode.value = ''
    claimStatus.value = 'Installation approved. Finish leo claim, then restart your manager and refresh installations.'
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to claim the installation.'
  }
  finally {
    busy.value = false
  }
}

async function detachInstallation() {
  if (!installation.value)
    return
  busy.value = true
  error.value = ''
  try {
    await installationRequest(`${installation.value.id}/detach`)
    redirect('/')
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to detach the installation.'
  }
  finally {
    busy.value = false
  }
}

async function addInstallation() {
  busy.value = true
  error.value = ''
  claimCode.value = ''
  try {
    const value = await installationRequest('claim-code')
    claimCode.value = value.code
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to add an installation.'
  }
  finally {
    busy.value = false
  }
}

async function oauth(provider: 'google' | 'github') {
  busy.value = true
  error.value = ''
  try {
    const start = await accountRequest(`oauth/${provider}/start`, {})
    try {
      sessionStorage.removeItem(officialReturnKey)
      if (state.installationId || claimPage) {
        const destination = claimPage ? '/claim' : `${window.location.pathname}${window.location.search}${window.location.hash}`
        sessionStorage.setItem(officialReturnKey, destination)
      }
    }
    catch {}

    window.location.assign(start.url)
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to reach this sign-in provider.'
    busy.value = false
  }
}

async function enableEmail() {
  busy.value = true
  error.value = ''
  try {
    if (challenge.value) {
      session.value = await accountRequest('verify', { challenge: challenge.value, code: code.value })
      challenge.value = ''
      code.value = ''
      methods.value = (await accountRequest('methods')).methods
    }
    else {
      const start = await accountRequest('email-code', { email: session.value?.account?.email })
      challenge.value = start.challenge
    }
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to enable email sign-in.'
  }
  finally {
    busy.value = false
  }
}

function openInstallation(value: { id: string, name: string }) {
  if (state.redirecting)
    return
  try {
    localStorage.setItem(`leo-current-installation:${session.value?.account?.id}`, value.id)
  }
  catch {}

  redirect(`/installations/${encodeURIComponent(value.id)}/`)
}

function closeInstallationMenu() {
  if (installationMenu.value)
    installationMenu.value.open = false
}

async function renameInstallation() {
  if (!installation.value || busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    const response = await fetch(`/api/installations/${encodeURIComponent(installation.value.id)}`, {
      method: 'PATCH',
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': state.csrf },
      body: JSON.stringify({ name: installationName.value }),
    })
    const result = await response.json()
    if (!response.ok)
      throw new Error(result.error || 'Unable to rename this installation.')
    installation.value.name = result.name
    editingName.value = false
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to rename this installation.'
  }
  finally {
    busy.value = false
  }
}

let availabilityTimer: ReturnType<typeof setTimeout> | undefined
let availabilityFailures = 0
let availabilityStopped = false

async function refreshAvailability() {
  clearTimeout(availabilityTimer)
  if (availabilityStopped || !session.value?.authenticated || document.hidden || state.redirecting)
    return
  const current = session.value
  try {
    const response = await fetch('/api/installations', { credentials: 'same-origin' })
    if (!response.ok)
      throw new Error('Installation status unavailable')
    const statuses = await response.json() as AccountSession['installations']
    if (session.value !== current || availabilityStopped)
      return
    for (const item of current.installations)
      item.online = statuses.find(status => status.id === item.id)?.online ?? false
    availabilityFailures = 0
  }
  catch {
    if (session.value === current) {
      for (const item of current.installations)
        item.online = false
    }

    availabilityFailures++
  }
  finally {
    if (!availabilityStopped)
      availabilityTimer = setTimeout(refreshAvailability, Math.min(15000, 3000 * 2 ** Math.min(availabilityFailures, 3)))
  }
}

watch(() => session.value?.authenticated, () => void refreshAvailability())

function visibleAvailability() {
  if (!document.hidden)
    void refreshAvailability()
}

document.addEventListener('visibilitychange', visibleAvailability)
onScopeDispose(() => {
  availabilityStopped = true
  clearTimeout(availabilityTimer)
  document.removeEventListener('visibilitychange', visibleAvailability)
})

function selectInstallation(event: Event) {
  const id = (event.target as HTMLSelectElement).value
  const selected = session.value?.installations.find(item => item.id === id)
  if (selected)
    openInstallation(selected)
}

function changeEmail() {
  challenge.value = ''
  code.value = ''
  error.value = ''
}

onMounted(async () => {
  const url = new URL(window.location.href)
  // OAuth callbacks return to the official root. Restore this tab's explicit
  // account or installation page before the session chooses an installation.
  try {
    const destination = sessionStorage.getItem(officialReturnKey)
    if (url.pathname === '/' && destination) {
      sessionStorage.removeItem(officialReturnKey)
      if (destination === '/claim' || /^\/installations\/[\w-]+\//.test(destination)) {
        const target = new URL(destination, url.origin)
        const signInError = url.searchParams.get('sign_in_error')
        if (signInError)
          target.searchParams.set('sign_in_error', signInError)
        redirect(target.href)
        return
      }
    }
  }
  catch {}

  await loadSession()
  if (url.searchParams.get('sign_in_error') === 'oauth') {
    error.value = 'Sign-in was cancelled or could not be verified. Try another method.'
    url.searchParams.delete('sign_in_error')
    window.history.replaceState(null, '', url)
  }
})
</script>

<template>
  <div v-if="session?.authenticated && installation && !showMethods" class="flex h-dvh min-h-0 flex-col bg-canvas text-ink">
    <header class="flex shrink-0 items-center gap-3 border-b border-line px-4 py-2 text-sm" aria-label="Current installation">
      <label v-if="session.installations.length > 1" class="min-w-0 max-w-full">
        <span class="sr-only">Current installation</span>
        <select :value="installation.id" :disabled="busy || state.redirecting" @change="selectInstallation">
          <option
            v-for="item in session.installations"
            :key="item.id"
            :value="item.id"
          >
            {{ item.name }} · {{ item.online ? 'Online' : 'Offline' }}
          </option>
        </select>
      </label>
      <span v-else class="min-w-0 truncate font-semibold" :title="installation.name">{{ installation.name }}</span>
      <span role="status" aria-label="Installation availability" class="shrink-0 text-xs text-muted">
        {{ installation.online ? 'Online' : 'Offline' }}
      </span>
      <details ref="installationMenu" class="relative ml-auto shrink-0">
        <summary class="cursor-pointer list-none rounded-lg border border-line px-3 py-2">
          Installation options
        </summary>
        <div class="absolute right-0 z-50 mt-2 grid w-52 gap-2 rounded-xl border border-line bg-surface p-2 shadow-lg" @click="closeInstallationMenu">
          <UiButton size="small" :disabled="busy" @click="editingName = !editingName; installationName = installation.name">
            Rename installation
          </UiButton>
          <UiButton size="small" :disabled="busy" @click="addInstallation">
            Add an installation
          </UiButton>
          <UiButton size="small" :disabled="busy" @click="confirmDetach = true">
            Detach installation
          </UiButton>
          <UiButton size="small" :disabled="busy" @click="openMethods">
            Sign-in methods
          </UiButton>
          <UiButton size="small" :disabled="busy" @click="signOut">
            Sign out
          </UiButton>
        </div>
      </details>
    </header>
    <div v-if="confirmDetach" class="grid gap-3 border-b border-line px-4 py-3">
      <p>Detach this installation? Access through Leo will stop. Its data stays on the machine, which can be claimed again.</p>
      <UiButton :disabled="busy" @click="detachInstallation">
        Confirm detachment
      </UiButton>
      <UiButton :disabled="busy" @click="confirmDetach = false">
        Cancel detachment
      </UiButton>
    </div>
    <form v-if="editingName" class="flex flex-wrap items-end gap-3 border-b border-line px-4 py-3" @submit.prevent="renameInstallation">
      <label>Installation name<input
        v-model="installationName"
        required
        maxlength="100"
        :disabled="busy"
      ></label>
      <UiButton type="submit" :disabled="busy">
        Save installation name
      </UiButton>
      <UiButton :disabled="busy" @click="editingName = false">
        Cancel
      </UiButton>
    </form>
    <div v-if="claimCode" class="grid gap-3 border-b border-line px-4 py-3">
      <label>Installation claim code<input :value="claimCode" readonly autocomplete="off"></label>
      <p class="text-sm text-muted">
        This code expires in 10 minutes.
      </p>
      <UiButton size="small" :disabled="busy" @click="loadSession">
        Refresh installations
      </UiButton>
      <UiButton size="small" :disabled="busy" @click="claimCode = ''">
        Close
      </UiButton>
    </div>
    <UiAlert v-if="error" class="mx-4 my-2">
      {{ error }}
    </UiAlert>
    <App />
  </div>
  <main v-else class="min-h-dvh bg-canvas text-ink px-6 py-10 grid place-items-center">
    <div class="absolute top-5 right-5">
      <ThemeControl compact />
    </div>
    <section class="w-full max-w-sm" aria-label="Leo account" :aria-busy="busy">
      <p class="font-heading text-brand text-3xl font-bold mb-10">
        leo
      </p>
      <template v-if="!ready">
        <p v-if="busy" role="status">
          Loading…
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton v-if="!busy" @click="loadSession">
          Try again
        </UiButton>
      </template>
      <template v-else-if="session?.authenticated && showMethods">
        <h1 class="font-heading text-2xl mb-4">
          Sign-in methods
        </h1>
        <p class="text-muted mb-4">
          {{ session.account?.email }} · Keep at least one sign-in method.
        </p>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <ul class="grid gap-4 mb-6">
          <li v-for="method in methods" :key="method.id" class="border border-line rounded-xl p-4 grid gap-2 min-w-0">
            <span>{{ methodNames[method.kind] }}</span>
            <span class="text-muted break-all">{{ method.label }}</span>
            <UiButton
              :disabled="busy || methods.length === 1"
              :aria-label="`Remove ${method.kind === 'passkey' ? method.label : `${methodNames[method.kind]} ${method.label}`}`"
              @click="removeMethod(method.id)"
            >
              Remove
            </UiButton>
          </li>
        </ul>
        <form v-if="options.passkeys" class="grid gap-3 mb-6" @submit.prevent="passkey(true)">
          <label>Passkey name<input
            v-model="passkeyName"
            required
            maxlength="80"
            :disabled="busy"
          ></label>
          <UiButton type="submit" :disabled="busy">
            Add passkey
          </UiButton>
        </form>
        <div class="grid gap-3 mb-6">
          <UiButton v-if="options.google && !methods.some(method => method.kind === 'google')" :disabled="busy" @click="oauth('google')">
            Add Google
          </UiButton>
          <UiButton v-if="options.github && !methods.some(method => method.kind === 'github')" :disabled="busy" @click="oauth('github')">
            Add GitHub
          </UiButton>
          <form v-if="!methods.some(method => method.kind === 'email')" class="grid gap-3" @submit.prevent="enableEmail">
            <label v-if="challenge">Email code<input
              v-model="code"
              autocomplete="one-time-code"
              inputmode="numeric"
              required
              maxlength="8"
              :disabled="busy"
            ></label>
            <UiButton type="submit" :disabled="busy">
              {{ challenge ? 'Confirm email code' : 'Enable email sign-in' }}
            </UiButton>
          </form>
        </div>
        <UiButton :disabled="busy" @click="showMethods = false; changeEmail()">
          Back to installations
        </UiButton>
      </template>
      <template v-else-if="session?.authenticated">
        <h1 class="font-heading text-2xl mb-4">
          {{ installation?.name || (session.installations.length ? 'Your installations' : 'No installations yet') }}
        </h1>
        <p class="text-muted mb-4">
          You’re signed in as {{ session.account?.email }}.
        </p>
        <div>
          <p v-if="!session.installations.length" class="text-muted mb-8">
            Your Leo account is ready. Choose Add an installation to get a claim code,
            then use it to connect a Leo installation on your machine. The installation
            will appear here; choose Refresh installations once it is connected.
          </p>
          <div class="grid gap-3 mb-6">
            <UiButton v-for="item in session.installations" :key="item.id" @click="openInstallation(item)">
              {{ item.name }} · {{ item.online ? 'Online' : 'Offline' }}
            </UiButton>
            <UiButton :disabled="busy" @click="addInstallation">
              Add an installation
            </UiButton>
            <UiButton :disabled="busy" @click="loadSession">
              Refresh installations
            </UiButton>
          </div>
          <form class="grid gap-3 mb-6" @submit.prevent="approveDevice">
            <label>Device claim code<input
              v-model="deviceCode"
              required
              maxlength="30"
              autocomplete="off"
              :disabled="busy"
            ></label>
            <p class="text-muted">
              Only approve a code displayed by a machine you control.
            </p>
            <UiButton type="submit" :disabled="busy">
              Claim this installation
            </UiButton>
            <p v-if="claimStatus" role="status">
              {{ claimStatus }}
            </p>
          </form>
          <div v-if="claimCode" class="grid gap-3 mb-6">
            <label>Installation claim code<input :value="claimCode" readonly autocomplete="off"></label>
            <p class="text-muted">
              This code expires in 10 minutes.
            </p>
          </div>
        </div>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton class="mb-3" :disabled="busy" @click="openMethods">
          Sign-in methods
        </UiButton>
        <UiButton :disabled="busy" @click="signOut">
          Sign out
        </UiButton>
      </template>
      <template v-else>
        <h1 class="font-heading text-2xl mb-4">
          {{ challenge ? 'Check your email' : 'Sign in to Leo' }}
        </h1>
        <p class="text-muted mb-8">
          {{ challenge ? `Enter the code sent to ${email}. It expires in 10 minutes.` : 'Create your Leo account or sign in with an email code.' }}
        </p>
        <div v-if="!challenge" class="grid gap-3 mb-6">
          <UiButton v-if="options.google" :disabled="busy" @click="oauth('google')">
            Continue with Google
          </UiButton>
          <UiButton v-if="options.github" :disabled="busy" @click="oauth('github')">
            Continue with GitHub
          </UiButton>
        </div>
        <form class="grid gap-5" @submit.prevent="submit">
          <label v-if="!challenge">Email address<input
            v-model="email"
            type="email"
            autocomplete="email"
            required
            maxlength="254"
            :disabled="busy"
          ></label>
          <label v-else>Email code<input
            v-model="code"
            type="text"
            inputmode="numeric"
            autocomplete="one-time-code"
            required
            maxlength="8"
            :disabled="busy"
          ></label>
          <UiAlert v-if="error">
            {{ error }}
          </UiAlert>
          <UiButton type="submit" variant="primary" :disabled="busy">
            {{ busy ? 'Please wait…' : challenge ? 'Sign in' : 'Send code' }}
          </UiButton>
          <UiButton v-if="!challenge && options.passkeys" :disabled="busy" @click="passkey(false)">
            Sign in with a passkey
          </UiButton>
          <UiButton v-if="challenge" :disabled="busy" @click="changeEmail">
            Use another email or request a new code
          </UiButton>
        </form>
      </template>
    </section>
  </main>
</template>
