<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { state } from '../api'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const emit = defineEmits<{ accepted: [installationId: string] }>()
const invitations = ref<Array<{
  id: string
  installationId: string
  installationName: string
  ownerEmail: string
}>>([])
const busy = ref(false)
const error = ref('')

async function load() {
  busy.value = true
  error.value = ''
  try {
    const response = await fetch('/api/account/invitations')
    if (!response.ok)
      throw new Error('Unable to load invitations.')
    invitations.value = await response.json()
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to load invitations.'
  }
  finally {
    busy.value = false
  }
}

async function accept(invitation: typeof invitations.value[number]) {
  busy.value = true
  error.value = ''
  try {
    const response = await fetch(`/api/account/invitations/${encodeURIComponent(invitation.id)}/accept`, {
      method: 'POST',
      headers: { 'X-CSRF-Token': state.csrf },
    })
    if (!response.ok) {
      const value = await response.json()
      throw new Error(value.error || 'Unable to accept invitation.')
    }

    emit('accepted', invitation.installationId)
  }
  catch (cause) {
    error.value = cause instanceof Error ? cause.message : 'Unable to accept invitation.'
  }
  finally {
    busy.value = false
  }
}

onMounted(load)
</script>

<template>
  <section aria-label="Pending invitations" class="grid gap-3 py-3">
    <h2 class="font-semibold">
      Pending invitations
    </h2>
    <p v-if="!invitations.length && !busy">
      No pending invitations.
    </p>
    <div v-for="invitation in invitations" :key="invitation.id" class="grid gap-2">
      <span>{{ invitation.installationName }}</span>
      <span class="text-muted break-all">Invited by {{ invitation.ownerEmail }}</span>
      <UiButton :disabled="busy" @click="accept(invitation)">
        Accept invitation
      </UiButton>
    </div>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <UiButton size="small" :disabled="busy" @click="load">
      Refresh invitations
    </UiButton>
  </section>
</template>
