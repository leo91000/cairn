<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { state } from '../api'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{ installationId: string }>()
const emit = defineEmits<{ close: [] }>()
const sharing = ref<{ members: Array<{ id: string, email: string }>, invitations: Array<{ id: string, email: string }> }>({ members: [], invitations: [] })
const email = ref('')
const busy = ref(false)
const error = ref('')

async function request(path = '', method = 'GET', body?: unknown) {
  const response = await fetch(`/api/installations/${encodeURIComponent(props.installationId)}/sharing${path}`, {
    method,
    headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': state.csrf },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  const value = response.status === 204 ? undefined : await response.json()
  if (!response.ok)
    throw new Error(value?.error || 'Unable to update sharing.')
  return value
}

async function update(path = '', method = 'GET', body?: unknown) {
  busy.value = true
  error.value = ''
  try {
    if (method !== 'GET')
      await request(path, method, body)
    sharing.value = await request()
    if (method === 'POST')
      email.value = ''
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to update sharing.'
  }
  finally {
    busy.value = false
  }
}

onMounted(() => update())
</script>

<template>
  <section aria-label="Installation sharing" class="grid gap-3 border-b border-line px-4 py-3">
    <h2 class="font-semibold">
      Installation sharing
    </h2>
    <p>Members use your coding-agent accounts and secrets.</p>
    <form class="flex flex-wrap items-end gap-3" @submit.prevent="update('/invitations', 'POST', { email })">
      <label>Invite by email<input
        v-model="email"
        type="email"
        required
        maxlength="254"
        :disabled="busy"
      ></label>
      <UiButton type="submit" :disabled="busy">
        Send invitation
      </UiButton>
    </form>
    <h3>Members</h3>
    <p v-if="!sharing.members.length">
      No members yet.
    </p>
    <div v-for="member in sharing.members" :key="member.id" class="flex flex-wrap items-center gap-3">
      <span class="break-all">{{ member.email }}</span>
      <UiButton :disabled="busy" :aria-label="`Remove ${member.email}`" @click="update(`/members/${encodeURIComponent(member.id)}`, 'DELETE')">
        Remove
      </UiButton>
    </div>
    <h3>Pending invitations</h3>
    <p v-if="!sharing.invitations.length">
      No pending invitations.
    </p>
    <div v-for="invitation in sharing.invitations" :key="invitation.id" class="flex flex-wrap items-center gap-3">
      <span class="break-all">{{ invitation.email }}</span>
      <UiButton :disabled="busy" :aria-label="`Cancel invitation to ${invitation.email}`" @click="update(`/invitations/${encodeURIComponent(invitation.id)}`, 'DELETE')">
        Cancel invitation
      </UiButton>
    </div>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <UiButton @click="emit('close')">
      Close sharing
    </UiButton>
  </section>
</template>
