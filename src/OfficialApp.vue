<script setup lang="ts">
import { onMounted, ref } from 'vue'
import ThemeControl from './components/ThemeControl.vue'
import UiAlert from './components/UiAlert.vue'
import UiButton from './components/UiButton.vue'

interface AccountSession {
  authenticated: boolean
  account: { id: string, email: string } | null
  csrf: string | null
  installations: unknown[]
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

async function accountRequest(route: string, body?: unknown) {
  const response = await fetch(`/api/account/${route}`, {
    method: body === undefined ? 'GET' : 'POST',
    credentials: 'same-origin',
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': session.value?.csrf || '' },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 401 && route === 'logout') {
    session.value = null
    return
  }

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
  busy.value = true
  error.value = ''
  try {
    await accountRequest('logout', {})
    session.value = null
    showMethods.value = false
    email.value = ''
    code.value = ''
    challenge.value = ''
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
    const start = await accountRequest(`passkeys/${route}/start`, register ? {} : { email: email.value })
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

async function oauth(provider: 'google' | 'github') {
  busy.value = true
  error.value = ''
  try {
    const start = await accountRequest(`oauth/${provider}/start`, {})
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

function changeEmail() {
  challenge.value = ''
  code.value = ''
  error.value = ''
}

onMounted(loadSession)
</script>

<template>
  <main class="min-h-dvh bg-canvas text-ink px-6 py-10 grid place-items-center">
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
          No installations yet
        </h1>
        <p class="text-muted mb-4">
          You’re signed in as {{ session.account?.email }}.
        </p>
        <p class="text-muted mb-8">
          Your Leo account is ready. Your installations will appear here when you add one.
        </p>
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
          <UiButton v-if="!challenge && options.passkeys" :disabled="busy || !email" @click="passkey(false)">
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
