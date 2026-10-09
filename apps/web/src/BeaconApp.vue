<script setup lang="ts">
import type { SelectOption } from './select'
import {
  computed,
  defineAsyncComponent,
  onMounted,
  onScopeDispose,
  ref,
  watch,
} from 'vue'
import { useRouter } from 'vue-router'
import { logoutAccount, redirect, state } from './api'
import App from './App.vue'
import AccountMenu from './components/AccountMenu.vue'
import AccountMethods from './components/AccountMethods.vue'
import AccountSecurity from './components/AccountSecurity.vue'
import BrandWordmark from './components/BrandWordmark.vue'
import Icon from './components/Icon.vue'
import InstallationSharing from './components/InstallationSharing.vue'
import Modal from './components/Modal.vue'
import NotificationSettings from './components/NotificationSettings.vue'
import PendingInvitations from './components/PendingInvitations.vue'
import ThemeControl from './components/ThemeControl.vue'
import UiAlert from './components/UiAlert.vue'
import UiButton from './components/UiButton.vue'
import VirtualSelect from './components/VirtualSelect.vue'
import { Settings } from './icons'
import Authorize from './views/Authorize.vue'

const PrivacyPolicy = defineAsyncComponent(() => import('./views/PrivacyPolicy.vue'))

interface AccountSession {
  authenticated: boolean
  account: { id: string, email: string } | null
  csrf: string | null
  installations: Array<{
    id: string
    name: string
    role: 'owner' | 'member'
    online: boolean
    updateRequired: boolean
  }>
}

const session = ref<AccountSession | null>(null)
const options = ref({ google: false, github: false, passkeys: false })
const showMethods = ref(false)
const showSecurity = ref(false)
const showIdentityConfirmation = ref(false)
const confirmationTitle = ref('')
const showNotifications = ref(false)
const ready = ref(false)
const email = ref('')
const code = ref('')
const challenge = ref('')
const busy = ref(false)
const methodBusy = ref(false)
const error = ref('')
const claimCode = ref('')
const installationCommand = computed(() => {
  const quote = (value: string) => `'${value.replaceAll('\'', '\'"\'"\'')}'`
  return `curl -fsSL ${quote(`${window.location.origin}/install.sh`)} | sudo bash -s -- --claim-code ${quote(claimCode.value)}`
})
const installationInstructions = 'This code expires in 10 minutes. Run this command on your Linux x86-64 machine. No domain, certificate, incoming port or S3 setup is needed.'
const showSharing = ref(false)
const showInvitations = ref(false)
const confirmLeave = ref(false)
const invitationsPage = new URLSearchParams(window.location.search).has('invitations')
const editingName = ref(false)
const installationName = ref('')
const deviceCode = ref('')
const deviceReview = ref<{
  code: string
  name: string
  fingerprint: string
  confirmation: string
} | null>(null)
const claimStatus = ref('')
const confirmDetach = ref(false)
const confirmForget = ref(false)
const installation = ref<{
  id: string
  name: string
  online: boolean
  updateRequired: boolean
  role: 'owner' | 'member'
} | null>(null)
const router = useRouter()
const installationOptions = computed<SelectOption[]>(() => (session.value?.installations ?? []).map(item => ({
  value: item.id,
  label: item.name,
  description: item.role === 'owner' ? 'Owner' : 'Member',
  keywords: [installationStatus(item)],
  status: {
    label: installationStatus(item),
    tone: item.updateRequired ? 'warning' : item.online ? 'success' : 'muted',
  },
})))
const beaconReturnKey = 'cairn-installation-return'
const authorizePage = window.location.pathname === '/authorize'
const claimPage = window.location.pathname === '/claim'
const privacyPage = window.location.pathname === '/privacy'

watch(deviceCode, () => deviceReview.value = null)

watch(session, (value) => {
  state.accountId = value?.account?.id || ''
  state.csrf = value?.csrf || ''
  state.authenticated = value?.authenticated || false
  state.ready = ready.value
  if (!value?.authenticated)
    return

  const requested = state.installationId
  installation.value = value.installations.find(item => item.id === requested) || null
  state.installationOnline = installation.value?.online
  state.installationUpdateRequired = installation.value?.updateRequired ?? false
  if (requested && !installation.value) {
    error.value = 'This installation is unavailable or no longer accessible to your Cairn account.'
    return
  }

  state.installationRole = installation.value?.role || 'owner'

  if (installation.value) {
    try {
      localStorage.setItem(`cairn-current-installation:${value.account?.id}`, installation.value.id)
    }
    catch {}
  }

  if (!requested && value.installations.length && !claimPage && !invitationsPage && !authorizePage) {
    let remembered = ''
    try {
      remembered = localStorage.getItem(`cairn-current-installation:${value.account?.id}`) || ''
    }
    catch {}

    openInstallation(value.installations.find(item => item.id === remembered) || value.installations[0]!)
  }
}, { flush: 'sync' })

watch(() => state.authenticated, (authenticated) => {
  if (!authenticated && session.value?.authenticated) {
    session.value = null
    installation.value = null
    showMethods.value = false
    showSecurity.value = false
    showIdentityConfirmation.value = false
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
    throw new Error(value?.error || 'Unable to reach Cairn. Please try again.')
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
    error.value = 'Unable to reach Cairn. Please try again.'
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
    error.value = cause instanceof Error ? cause.message : 'Unable to reach Cairn. Please try again.'
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
    showSecurity.value = false
    showIdentityConfirmation.value = false
    installation.value = null
    claimCode.value = ''
    deviceCode.value = ''
    deviceReview.value = null
    claimStatus.value = ''
    confirmDetach.value = false
    confirmForget.value = false
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

function openMethods() {
  if (installation.value)
    void router.push('/settings/account')
  else
    showMethods.value = true
}

function openIdentityConfirmation(title: string) {
  confirmationTitle.value = title
  showIdentityConfirmation.value = true
}

async function passkey() {
  busy.value = true
  error.value = ''
  try {
    if (!window.PublicKeyCredential?.parseCreationOptionsFromJSON || !PublicKeyCredential.parseRequestOptionsFromJSON)
      throw new Error('Passkeys are unavailable in this browser. Use another sign-in method.')
    const start = await accountRequest('passkeys/login/start', {})
    const credential = await navigator.credentials.get({ publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(start.options.publicKey) })
    if (!(credential instanceof PublicKeyCredential))
      throw new Error('Passkey operation cancelled. Please try again.')
    const result = await accountRequest('passkeys/login/finish', {
      challenge: start.challenge,
      credential: credential.toJSON(),
    })
    session.value = result
    challenge.value = ''
    code.value = ''
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

async function installationRequest(route: string, body?: unknown, method: 'POST' | 'DELETE' = 'POST') {
  const response = await fetch(`/api/installations/${route}`, {
    method,
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

async function leaveInstallation() {
  if (!installation.value)
    return
  busy.value = true
  error.value = ''
  try {
    const response = await fetch(`/api/installations/${encodeURIComponent(installation.value.id)}/sharing/membership`, {
      method: 'DELETE',
      headers: { 'X-CSRF-Token': state.csrf },
    })
    if (!response.ok) {
      const value = await response.json()
      throw new Error(value.error || 'Unable to leave this installation.')
    }

    redirect('/')
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to leave this installation.'
  }
  finally {
    busy.value = false
  }
}

async function reviewDevice() {
  busy.value = true
  error.value = ''
  claimStatus.value = ''
  deviceReview.value = null
  try {
    const value = await installationRequest('device-claim/preview', { code: deviceCode.value })
    deviceReview.value = { ...value, code: deviceCode.value }
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to review the installation.'
  }
  finally {
    busy.value = false
  }
}

async function approveDevice() {
  if (!deviceReview.value)
    return
  busy.value = true
  error.value = ''
  claimStatus.value = ''
  try {
    await installationRequest('device-claim', { code: deviceReview.value.code, confirmation: deviceReview.value.confirmation })
    deviceCode.value = ''
    deviceReview.value = null
    claimStatus.value = 'Installation approved. Finish cairn claim, then restart your manager and refresh installations.'
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

async function forgetInstallation() {
  if (!installation.value)
    return
  busy.value = true
  error.value = ''
  try {
    await installationRequest(encodeURIComponent(installation.value.id), undefined, 'DELETE')
    redirect('/')
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to revoke the installation.'
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
      sessionStorage.removeItem(beaconReturnKey)
      if (state.installationId || claimPage || authorizePage) {
        const destination = claimPage ? '/claim' : `${window.location.pathname}${window.location.search}${window.location.hash}`
        sessionStorage.setItem(beaconReturnKey, destination)
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

function openInstallation(value: { id: string, name: string }) {
  if (state.redirecting)
    return
  try {
    localStorage.setItem(`cairn-current-installation:${session.value?.account?.id}`, value.id)
  }
  catch {}

  redirect(`/installations/${encodeURIComponent(value.id)}/`)
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
let availabilityRequest: AbortController | undefined
let availabilityFailures = 0
let availabilityStopped = false

async function refreshAvailability() {
  clearTimeout(availabilityTimer)
  if (availabilityStopped || !session.value?.authenticated || document.hidden || state.redirecting || state.signingOut)
    return
  availabilityRequest?.abort()
  const controller = new AbortController()
  availabilityRequest = controller
  const current = session.value
  try {
    const response = await fetch('/api/installations', { credentials: 'same-origin', signal: controller.signal })
    if (controller.signal.aborted)
      return
    if (response.status === 401 && session.value === current && !availabilityStopped) {
      availabilityStopped = true
      redirect(window.location.href)
      return
    }

    if (!response.ok)
      throw new Error('Installation status unavailable')
    const statuses = await response.json() as AccountSession['installations']
    if (session.value !== current || availabilityStopped)
      return
    for (const item of current.installations) {
      const status = statuses.find(status => status.id === item.id)
      item.online = status?.online ?? false
      item.updateRequired = status?.updateRequired ?? false
    }

    const currentInstallation = current.installations.find(item => item.id === state.installationId)
    state.installationOnline = currentInstallation?.online
    state.installationUpdateRequired = currentInstallation?.updateRequired ?? false

    availabilityFailures = 0
  }
  catch {
    if (controller.signal.aborted)
      return
    if (session.value === current) {
      for (const item of current.installations)
        item.online = false
      state.installationOnline = false
    }

    availabilityFailures++
  }
  finally {
    if (availabilityRequest === controller)
      availabilityRequest = undefined
    if (!availabilityStopped && !controller.signal.aborted && !state.signingOut && session.value?.authenticated)
      availabilityTimer = setTimeout(refreshAvailability, Math.min(15000, 3000 * 2 ** Math.min(availabilityFailures, 3)))
  }
}

watch(() => [session.value?.authenticated, state.signingOut], () => {
  if (state.signingOut || !session.value?.authenticated)
    availabilityRequest?.abort()
  void refreshAvailability()
}, { flush: 'sync' })

function visibleAvailability() {
  if (!document.hidden)
    void refreshAvailability()
}

document.addEventListener('visibilitychange', visibleAvailability)
onScopeDispose(() => {
  availabilityStopped = true
  availabilityRequest?.abort()
  clearTimeout(availabilityTimer)
  document.removeEventListener('visibilitychange', visibleAvailability)
})

function installationStatus(value: { online: boolean, updateRequired: boolean }) {
  if (value.updateRequired)
    return 'Mise à jour nécessaire'
  return value.online ? 'Online' : 'Offline'
}

function selectInstallation(id: string) {
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
  // The public privacy policy never needs or reveals the account session.
  if (privacyPage)
    return

  const url = new URL(window.location.href)
  // OAuth callbacks return to the beacon root. Restore this tab's explicit
  // account or installation page before the session chooses an installation.
  try {
    const destination = sessionStorage.getItem(beaconReturnKey)
    if (url.pathname === '/' && destination) {
      sessionStorage.removeItem(beaconReturnKey)
      if (destination === '/claim' || /^\/authorize(?:\?|$)/.test(destination) || /^\/installations\/[\w-]+\//.test(destination)) {
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
  const signInError = url.searchParams.get('sign_in_error')
  const oauthErrors: Record<string, string> = {
    oauth: 'Sign-in was cancelled or could not be verified. Try another method.',
    oauth_link_required: 'A Cairn account already uses this email. Sign in with an email code, then link Google or GitHub from Sign-in methods.',
    oauth_proof: 'Confirm your identity with an email code or passkey before linking a sign-in method. Then try linking again.',
    oauth_unavailable: 'Sign-in provider unavailable. Please try again or use another sign-in method.',
  }
  if (signInError && Object.hasOwn(oauthErrors, signInError)) {
    if (signInError === 'oauth_proof' && session.value?.authenticated)
      await openMethods()
    error.value = oauthErrors[signInError]!
    url.searchParams.delete('sign_in_error')
    window.history.replaceState(null, '', url)
  }
})
</script>

<template>
  <PrivacyPolicy v-if="privacyPage" />
  <main v-else-if="session?.authenticated && authorizePage" class="min-h-dvh bg-canvas text-ink px-6 py-10">
    <Authorize />
  </main>
  <div v-else-if="session?.authenticated && installation" class="flex h-dvh min-h-0 flex-col bg-canvas text-ink">
    <header class="flex shrink-0 items-center gap-3 border-b border-line px-4 py-2 text-sm phone:gap-2 phone:px-3" aria-label="Current installation">
      <VirtualSelect
        v-if="session.installations.length > 1"
        :model-value="installation.id"
        label="Current installation"
        :options="installationOptions"
        :disabled="busy || methodBusy || state.redirecting"
        compact
        hide-label
        action-label="Add an installation"
        class="min-w-0! max-w-80! flex-1"
        @update:model-value="selectInstallation"
        @action="addInstallation"
      />
      <span v-else class="min-w-0 truncate font-semibold" :title="installation.name">{{ installation.name }}</span>
      <span class="ml-auto flex shrink-0 items-center gap-2 text-xs text-muted" :title="`${installationStatus(installation)} · ${state.transportRoute === 'direct' ? 'Direct' : 'Relais'}`">
        <span
          role="status"
          aria-label="Installation availability"
          class="flex items-center gap-1.5"
          :class="installation.updateRequired ? 'text-warning' : installation.online ? 'text-success' : 'text-muted'"
        ><span aria-hidden="true" class="size-1.5 shrink-0 rounded-full bg-current" :class="session.installations.length > 1 ? 'phone:hidden' : ''" /><span class="phone:sr-only">{{ installationStatus(installation) }}</span></span>
        <span aria-hidden="true" class="h-3 w-px bg-line phone:hidden" />
        <span
          role="status"
          aria-label="Connection route"
          :data-transport-route="state.transportRoute"
          class="text-xs text-muted"
        >{{ state.transportRoute === 'direct' ? 'Direct' : 'Relais' }}</span>
      </span>
      <UiButton
        class="size-11 p-0"
        aria-label="Installation settings"
        title="Installation settings"
        :disabled="busy || methodBusy || state.redirecting"
        @click="router.push('/settings/installation')"
      >
        <Icon :name="Settings" :size="18" />
      </UiButton>
      <AccountMenu
        :email="session.account?.email || ''"
        :disabled="busy || methodBusy || state.redirecting"
        @settings="router.push('/settings/account')"
        @sign-out="signOut"
      />
    </header>
    <div v-if="claimCode" class="grid gap-3 border-b border-line px-4 py-3">
      <label>Installation command<textarea
        :value="installationCommand"
        readonly
        autocomplete="off"
        rows="4"
        spellcheck="false"
        class="w-full font-mono text-xs"
      /></label>
      <label>Installation claim code<input :value="claimCode" readonly autocomplete="off"></label>
      <p class="text-sm text-muted">
        {{ installationInstructions }}
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
    <UiAlert v-if="installation.updateRequired" class="mx-4 my-2">
      Mise à jour nécessaire. This installation must finish updating before it can be used here.
    </UiAlert>
    <App v-if="!installation.updateRequired || router.currentRoute.value.path.startsWith('/settings')">
      <template #settings-account>
        <section aria-label="Profile" class="border-b border-line py-6">
          <h2>Profile</h2>
          <p class="mt-3 break-all text-muted">
            {{ session.account?.email }}
          </p>
        </section>
        <section aria-label="Notifications" class="border-b border-line py-6">
          <h2 class="mb-4">
            Notifications
          </h2>
          <NotificationSettings />
        </section>
        <div class="border-b border-line py-6">
          <AccountMethods
            :email="session.account?.email || ''"
            :options="options"
            :request="accountRequest"
            :busy="busy"
            embedded
            @busy="methodBusy = $event"
            @clear-error="error = ''"
            @oauth="oauth"
            @session-updated="session = $event"
            @signed-out="redirect('/')"
          />
        </div>
        <section aria-label="Account security" class="border-b border-line py-6">
          <AccountSecurity
            :email="session.account?.email || ''"
            :passkeys="options.passkeys"
            embedded
            @confirm-identity="openIdentityConfirmation('Confirm identity before revoking other devices')"
            @signed-out="redirect('/')"
          />
        </section>
      </template>
      <template #settings-installation>
        <section aria-label="General" class="border-b border-line py-6">
          <h2>General</h2>
          <dl class="mt-4 grid grid-cols-[auto_minmax(0,1fr)] gap-x-5 gap-y-3 text-sm">
            <dt class="text-muted">
              Name
            </dt><dd class="break-words">
              {{ installation.name }}
            </dd>
            <dt class="text-muted">
              Owner
            </dt>
            <dd class="break-all">
              {{ installation.role === 'owner' ? session.account?.email : 'The account that shared this installation' }}
            </dd>
            <dt class="text-muted">
              Your access
            </dt><dd>{{ installation.role === 'owner' ? 'Owner' : 'Member' }}</dd>
          </dl>
          <div class="mt-5 flex flex-wrap gap-3">
            <UiButton
              v-if="installation.role === 'owner'"
              size="small"
              :disabled="busy"
              @click="editingName = !editingName; installationName = installation.name"
            >
              Rename installation
            </UiButton>
            <UiButton size="small" :disabled="busy" @click="addInstallation">
              Add an installation
            </UiButton>
          </div>
          <form v-if="editingName" class="mt-5 flex flex-wrap items-end gap-3" @submit.prevent="renameInstallation">
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
        </section>
        <section aria-label="Members and invitations" class="border-b border-line py-6">
          <h2 class="mb-4">
            Members &amp; invitations
          </h2>
          <div class="flex flex-wrap gap-3">
            <UiButton
              v-if="installation.role === 'owner'"
              size="small"
              :disabled="busy"
              @click="showSharing = !showSharing"
            >
              Share installation
            </UiButton>
            <UiButton size="small" :disabled="busy" @click="showInvitations = !showInvitations">
              Invitations
            </UiButton>
          </div>
          <InstallationSharing v-if="showSharing && installation.role === 'owner'" :installation-id="installation.id" @close="showSharing = false" />
          <PendingInvitations v-if="showInvitations" @accepted="id => openInstallation({ id, name: '' })" />
        </section>
      </template>
      <template #settings-installation-heading>
        <h2 class="text-xl font-heading break-words">
          Installation “{{ installation.name }}”
        </h2>
      </template>
      <template #settings-sensitive>
        <section aria-label="Installation access" class="rounded-xl border border-danger/30 bg-danger-surface p-5">
          <h2 class="text-danger">
            Installation access
          </h2>
          <p class="my-4 text-sm text-muted">
            These actions change access to {{ installation.name }}. Data stays on the machine.
          </p>
          <div class="flex flex-wrap gap-3">
            <template v-if="installation.role === 'owner'">
              <UiButton variant="danger-outline" :disabled="busy" @click="confirmDetach = true">
                Detach installation
              </UiButton>
              <UiButton variant="danger-outline" :disabled="busy" @click="confirmForget = true">
                Revoke and forget installation
              </UiButton>
            </template>
            <UiButton
              v-else
              variant="danger-outline"
              :disabled="busy"
              @click="confirmLeave = true"
            >
              Leave installation
            </UiButton>
          </div>
          <div v-if="confirmLeave" class="grid gap-3 border-b border-line px-4 py-3">
            <p>Leave this shared installation? You will need a new invitation to return.</p>
            <UiButton variant="danger" :disabled="busy" @click="leaveInstallation">
              Confirm leaving
            </UiButton>
            <UiButton :disabled="busy" @click="confirmLeave = false">
              Cancel leaving
            </UiButton>
          </div>
          <div v-if="confirmDetach" class="grid gap-3 border-b border-line px-4 py-3">
            <p>Detach this installation? Access through Cairn will stop. Its data stays on the machine, which can be claimed again.</p>
            <UiButton :disabled="busy" @click="openIdentityConfirmation('Confirm identity before detaching this installation')">
              Confirm identity
            </UiButton>
            <UiButton variant="danger" :disabled="busy" @click="detachInstallation">
              Confirm detachment
            </UiButton>
            <UiButton :disabled="busy" @click="confirmDetach = false">
              Cancel detachment
            </UiButton>
          </div>
          <div v-if="confirmForget" class="grid gap-3 border-b border-line px-4 py-3">
            <p>Revoke this installation permanently? Its credentials and shared access will stop working. Claim the machine again to return.</p>
            <p>Its data stays on the machine.</p>
            <UiButton :disabled="busy" @click="openIdentityConfirmation('Confirm identity before revoking this installation')">
              Confirm identity
            </UiButton>
            <UiButton variant="danger" :disabled="busy" @click="forgetInstallation">
              Confirm revocation
            </UiButton>
            <UiButton :disabled="busy" @click="confirmForget = false">
              Cancel revocation
            </UiButton>
          </div>
        </section>
      </template>
    </App>
    <Modal v-if="showIdentityConfirmation" title="Confirm identity" @close="showIdentityConfirmation = false">
      <div class="p-6">
        <AccountSecurity
          :email="session.account?.email || ''"
          :passkeys="options.passkeys"
          confirmation-only
          :confirmation-title="confirmationTitle"
          return-label="Back to settings"
          @close="showIdentityConfirmation = false"
          @confirmed="showIdentityConfirmation = false; error = ''"
          @signed-out="redirect('/')"
        />
      </div>
    </Modal>
  </div>
  <main v-else class="min-h-dvh bg-canvas text-ink px-6 py-10 grid place-items-center">
    <div class="absolute top-5 right-5">
      <ThemeControl compact />
    </div>
    <section class="w-full max-w-sm" aria-label="Cairn account" :aria-busy="busy">
      <BrandWordmark class="mb-10" />
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
      <AccountSecurity
        v-else-if="session?.authenticated && (showSecurity || showIdentityConfirmation)"
        :email="session.account?.email || ''"
        :passkeys="options.passkeys"
        :confirmation-only="showIdentityConfirmation"
        :confirmation-title="confirmationTitle"
        :return-label="showSecurity ? 'Back to account security' : 'Back to installation'"
        @close="showIdentityConfirmation ? showIdentityConfirmation = false : showSecurity = false"
        @confirm-identity="openIdentityConfirmation('Confirm identity before revoking other devices')"
        @confirmed="showIdentityConfirmation = false; error = ''"
        @signed-out="redirect('/')"
      />
      <AccountMethods
        v-else-if="session?.authenticated && showMethods"
        :external-error="error"
        :email="session.account?.email || ''"
        :options="options"
        :request="accountRequest"
        :busy="busy"
        @busy="methodBusy = $event"
        @clear-error="error = ''"
        @oauth="oauth"
        @session-updated="session = $event"
        @signed-out="redirect('/')"
        @close="showMethods = false; changeEmail()"
      />
      <template v-else-if="session?.authenticated">
        <h1 class="font-heading text-2xl mb-4">
          {{ installation?.name || (session.installations.length ? 'Your installations' : 'No installations yet') }}
        </h1>
        <p class="text-muted mb-4">
          You’re signed in as {{ session.account?.email }}.
        </p>
        <PendingInvitations @accepted="id => openInstallation({ id, name: '' })" />
        <div>
          <p v-if="!session.installations.length" class="text-muted mb-8">
            Your Cairn account is ready. Choose Add an installation to get a command,
            then run it on your Linux x86-64 machine. The installation
            will appear here; choose Refresh installations once it is connected.
          </p>
          <div class="grid gap-3 mb-6">
            <UiButton v-for="item in session.installations" :key="item.id" @click="openInstallation(item)">
              {{ item.name }} · {{ installationStatus(item) }}
            </UiButton>
            <UiButton :disabled="busy" @click="addInstallation">
              Add an installation
            </UiButton>
            <UiButton :disabled="busy" @click="loadSession">
              Refresh installations
            </UiButton>
          </div>
          <form class="grid gap-3 mb-6" @submit.prevent="reviewDevice">
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
              Review installation
            </UiButton>
            <div
              v-if="deviceReview"
              role="dialog"
              aria-label="Confirm installation claim"
              class="grid gap-3 rounded-xl border border-line p-4"
            >
              <p>Claim {{ deviceReview.name }}?</p>
              <label>Installation fingerprint<input :value="deviceReview.fingerprint" readonly></label>
              <p class="text-muted">
                Compare this fingerprint with cairn claim in the terminal on a machine you control.
                This machine will store your conversations, coding-agent accounts and secrets.
                Do not approve a code sent by someone else, even if they ask you to sign in.
              </p>
              <UiButton :disabled="busy" @click="approveDevice">
                Claim this installation
              </UiButton>
              <UiButton :disabled="busy" @click="deviceReview = null">
                Cancel claim
              </UiButton>
            </div>
            <p v-if="claimStatus" role="status">
              {{ claimStatus }}
            </p>
          </form>
          <div v-if="claimCode" class="grid gap-3 mb-6">
            <label>Installation command<textarea
              :value="installationCommand"
              readonly
              autocomplete="off"
              rows="4"
              spellcheck="false"
              class="w-full font-mono text-xs"
            /></label>
            <label>Installation claim code<input :value="claimCode" readonly autocomplete="off"></label>
            <p class="text-muted">
              {{ installationInstructions }}
            </p>
          </div>
        </div>
        <UiAlert v-if="error">
          {{ error }}
        </UiAlert>
        <UiButton class="mb-3" :disabled="busy" @click="showNotifications = true">
          Notifications
        </UiButton>
        <UiButton class="mb-3" :disabled="busy" @click="openMethods">
          Sign-in methods
        </UiButton>
        <UiButton class="mb-3" :disabled="busy" @click="showSecurity = true">
          Account security
        </UiButton>
        <UiButton :disabled="busy" @click="signOut">
          Sign out
        </UiButton>
      </template>
      <template v-else>
        <h1 class="font-heading text-2xl mb-4">
          {{ challenge ? 'Check your email' : 'Sign in to Cairn' }}
        </h1>
        <p class="text-muted mb-8">
          {{ challenge ? `Enter the code sent to ${email}. It expires in 10 minutes.` : 'Create your Cairn account or sign in with an email code.' }}
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
          <UiButton v-if="!challenge && options.passkeys" :disabled="busy" @click="passkey()">
            Sign in with a passkey
          </UiButton>
          <UiButton v-if="challenge" :disabled="busy" @click="changeEmail">
            Use another email or request a new code
          </UiButton>
        </form>
        <p class="mt-8 text-sm text-muted">
          <a class="underline underline-offset-2 hover:text-ink" href="/privacy">Privacy Policy</a>
        </p>
      </template>
    </section>
  </main>
  <Modal v-if="session?.authenticated && showNotifications" title="Notifications" @close="showNotifications = false">
    <div class="p-6 phone:p-4">
      <NotificationSettings />
    </div>
  </Modal>
</template>
