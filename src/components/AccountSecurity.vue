<script setup lang="ts">
import { onMounted, onScopeDispose, ref } from 'vue'
import { state } from '../api'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

interface AccountDevice {
  id: string
  device: string
  createdAt: string
  expiresAt: string
  current: boolean
}

const props = defineProps<{
  email: string
  passkeys: boolean
  confirmationOnly?: boolean
  confirmationTitle?: string
  returnLabel?: string
}>()
const emit = defineEmits<{ close: [], signedOut: [], confirmed: [] }>()
const confirmDelete = ref(false)
const confirmation = ref('')
const sessions = ref<AccountDevice[]>([])
const busy = ref(false)
const error = ref('')
const challenge = ref('')
const code = ref('')
const identityConfirmed = ref(false)
let confirmationTimer: ReturnType<typeof setTimeout> | undefined
onScopeDispose(() => clearTimeout(confirmationTimer))

async function request(route: string, method = 'GET', body?: unknown) {
  const response = await fetch(`/api/account/${route}`, {
    method,
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': state.csrf },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 401) {
    const confirmingIdentity = route.startsWith('reauth/') || route.startsWith('passkeys/reauth/')
    if (confirmingIdentity) {
      // A rejected proof also uses 401; sign out only if the session is gone.
      const current = await fetch('/api/account/session').then((response) => {
        return response.headers.get('content-type')?.includes('application/json') ? response.json() : undefined
      }).catch(() => undefined)
      if (current?.authenticated === false)
        emit('signedOut')
    }
    else {
      emit('signedOut')
    }
  }

  const hasJson = response.headers.get('content-type')?.includes('application/json')
  const value = response.status === 204 || !hasJson ? undefined : await response.json().catch(() => undefined)
  if (!response.ok)
    throw new Error(value?.error || 'Unable to update account security.')
  if (response.status !== 204 && value === undefined)
    throw new Error('Unable to update account security.')
  return value
}

async function update(route?: string, current = false) {
  busy.value = true
  error.value = ''
  try {
    if (route)
      await request(route, route === 'sessions/revoke-others' ? 'POST' : 'DELETE')
    if (current) {
      emit('signedOut')
      return
    }

    sessions.value = (await request('sessions')).sessions
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to update account security.'
  }
  finally {
    busy.value = false
  }
}

function resetDeletion() {
  confirmDelete.value = false
  confirmation.value = ''
  challenge.value = ''
  code.value = ''
  identityConfirmed.value = false
  clearTimeout(confirmationTimer)
}

async function confirmIdentity(method: 'send-code' | 'email' | 'passkey') {
  busy.value = true
  error.value = ''
  try {
    if (method === 'send-code') {
      challenge.value = (await request('email-code', 'POST', { email: props.email })).challenge
      code.value = ''
      return
    }

    if (method === 'email') {
      await request('reauth/email', 'POST', { challenge: challenge.value, code: code.value })
    }
    else {
      if (!window.PublicKeyCredential?.parseRequestOptionsFromJSON)
        throw new Error('Passkeys are unavailable in this browser. Use an email code.')
      const start = await request('passkeys/reauth/start', 'POST', {})
      const credential = await navigator.credentials.get({ publicKey: PublicKeyCredential.parseRequestOptionsFromJSON(start.options.publicKey) })
      if (!(credential instanceof PublicKeyCredential))
        throw new Error('Passkey confirmation cancelled. Please try again.')
      await request('passkeys/reauth/finish', 'POST', { challenge: start.challenge, credential: credential.toJSON() })
    }

    challenge.value = ''
    code.value = ''
    if (props.confirmationOnly) {
      emit('confirmed')
      return
    }

    identityConfirmed.value = true
    clearTimeout(confirmationTimer)
    confirmationTimer = setTimeout(() => identityConfirmed.value = false, 5 * 60 * 1000)
  }
  catch (cause) {
    error.value = cause instanceof DOMException
      ? 'Passkey confirmation cancelled or unavailable. You can use an email code.'
      : cause instanceof Error ? cause.message : 'Unable to confirm your identity.'
  }
  finally {
    busy.value = false
  }
}

async function deleteAccount() {
  if (!identityConfirmed.value)
    return
  busy.value = true
  error.value = ''
  try {
    await request('delete', 'POST', { email: confirmation.value })
    emit('signedOut')
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to delete account.'
  }
  finally {
    busy.value = false
  }
}

function date(value: string) {
  return new Date(value).toLocaleString()
}

onMounted(() => {
  if (!props.confirmationOnly)
    void update()
})
</script>

<template>
  <div class="grid gap-4" :aria-busy="busy">
    <h1 class="font-heading text-2xl">
      {{ props.confirmationOnly ? 'Confirm identity' : 'Account security' }}
    </h1>
    <template v-if="!props.confirmationOnly">
      <h2 class="font-semibold">
        Active sessions
      </h2>
      <p class="text-muted">
        Revoke a lost device to sign it out and close its live views. Agent work continues on your installations.
      </p>
      <ul class="grid gap-3">
        <li v-for="session in sessions" :key="session.id" class="border border-line rounded-xl p-4 grid gap-2 min-w-0">
          <strong v-if="session.current">This device</strong>
          <span class="break-all">{{ session.device }}</span>
          <span class="text-muted">Signed in {{ date(session.createdAt) }}</span>
          <span class="text-muted">Expires {{ date(session.expiresAt) }}</span>
          <UiButton :disabled="busy" :aria-label="`Revoke ${session.current ? 'this device' : session.device}`" @click="update(`sessions/${encodeURIComponent(session.id)}`, session.current)">
            {{ session.current ? 'Sign out this device' : 'Revoke session' }}
          </UiButton>
        </li>
      </ul>
      <UiButton :disabled="busy || !sessions.some(session => !session.current)" @click="update('sessions/revoke-others')">
        Revoke other devices
      </UiButton>
    </template>
    <section class="border-t border-line pt-4 grid gap-3">
      <h2 class="font-semibold">
        {{ props.confirmationOnly ? (props.confirmationTitle || 'Confirm identity before changing sign-in methods') : 'Delete account' }}
      </h2>
      <UiButton v-if="!confirmDelete && !props.confirmationOnly" :disabled="busy" @click="confirmDelete = true">
        Delete account
      </UiButton>
      <form v-else class="grid gap-3" @submit.prevent="props.confirmationOnly ? confirmIdentity(challenge ? 'email' : 'send-code') : deleteAccount()">
        <template v-if="!props.confirmationOnly">
          <p>Your installations become unclaimed. Their data stays on their machines; all members lose access.</p>
          <p class="text-muted">
            Your sessions and sign-in methods are removed. Running agent work continues. You can claim the installations again from their machines.
          </p>
          <label>Account email to confirm deletion<input
            v-model="confirmation"
            type="email"
            autocomplete="off"
            required
            :disabled="busy"
          ></label>
          <p class="text-muted break-all">
            Enter {{ props.email }} to confirm.
          </p>
        </template>
        <p v-if="identityConfirmed" role="status">
          Identity confirmed for five minutes.
        </p>
        <template v-else>
          <p>Confirm your identity with an email code or passkey to continue.</p>
          <UiButton v-if="props.passkeys" :disabled="busy" @click="confirmIdentity('passkey')">
            Confirm with a passkey
          </UiButton>
          <UiButton :disabled="busy" @click="confirmIdentity('send-code')">
            {{ challenge ? 'Request another confirmation code' : 'Send confirmation code' }}
          </UiButton>
          <template v-if="challenge">
            <label>Confirmation code<input
              v-model="code"
              autocomplete="one-time-code"
              inputmode="numeric"
              :disabled="busy"
            ></label>
            <UiButton :disabled="busy || !code.trim()" @click="confirmIdentity('email')">
              Verify confirmation code
            </UiButton>
          </template>
        </template>
        <UiButton v-if="!props.confirmationOnly" type="submit" :disabled="busy || !identityConfirmed || confirmation.trim() !== props.email">
          Confirm account deletion
        </UiButton>
        <UiButton v-if="!props.confirmationOnly" :disabled="busy" @click="resetDeletion">
          Cancel deletion
        </UiButton>
      </form>
    </section>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <UiButton :disabled="busy" @click="emit('close')">
      {{ props.confirmationOnly ? (props.returnLabel || 'Back to sign-in methods') : 'Back to installations' }}
    </UiButton>
  </div>
</template>
