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

const session = ref<AccountSession | null>(null)
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
  const value = await response.json()
  if (!response.ok)
    throw new Error(value.error || 'Unable to reach Leo. Please try again.')
  return value
}

async function loadSession() {
  busy.value = true
  error.value = ''
  try {
    session.value = await accountRequest('session')
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
          <UiButton v-if="challenge" :disabled="busy" @click="changeEmail">
            Use another email or request a new code
          </UiButton>
        </form>
      </template>
    </section>
  </main>
</template>
