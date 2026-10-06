<script setup lang="ts">
import { onMounted, ref } from 'vue'
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

const props = defineProps<{ email: string }>()
const emit = defineEmits<{ close: [], signedOut: [] }>()
const confirmDelete = ref(false)
const confirmation = ref('')
const sessions = ref<AccountDevice[]>([])
const busy = ref(false)
const error = ref('')

async function request(route: string, method = 'GET', body?: unknown) {
  const response = await fetch(`/api/account/${route}`, {
    method,
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': state.csrf },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const value = response.status === 204 ? undefined : await response.json()
  if (response.status === 401)
    emit('signedOut')
  if (!response.ok)
    throw new Error(value?.error || 'Unable to update account security.')
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

async function deleteAccount() {
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

onMounted(() => update())
</script>

<template>
  <div class="grid gap-4" :aria-busy="busy">
    <h1 class="font-heading text-2xl">
      Account security
    </h1>
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
    <section class="border-t border-line pt-4 grid gap-3">
      <h2 class="font-semibold">
        Delete account
      </h2>
      <UiButton v-if="!confirmDelete" :disabled="busy" @click="confirmDelete = true">
        Delete account
      </UiButton>
      <form v-else class="grid gap-3" @submit.prevent="deleteAccount">
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
        <UiButton type="submit" :disabled="busy || confirmation.trim() !== props.email">
          Confirm account deletion
        </UiButton>
        <UiButton :disabled="busy" @click="confirmDelete = false; confirmation = ''">
          Cancel deletion
        </UiButton>
      </form>
    </section>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <UiButton :disabled="busy" @click="emit('close')">
      Back to installations
    </UiButton>
  </div>
</template>
