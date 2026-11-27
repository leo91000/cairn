<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { api } from '../api'
import OAuthStatusCard from '../components/OAuthStatusCard.vue'

const props = defineProps<{ parameters?: Record<string, string>, embedded?: boolean }>()
const error = ref('')
const completed = ref(false)
const parameters = props.parameters ?? Object.fromEntries(new URLSearchParams(location.search))
const cardState = computed(() => error.value ? 'error' : completed.value ? 'success' : 'loading')
const cardTitle = computed(() => error.value ? 'Sign-in could not be completed' : completed.value ? 'Sign-in complete' : 'Completing sign-in…')
const message = computed(() => completed.value ? 'Signed in. Return to the account editor in Cairn and save the account.' : 'Completing sign-in…')
// Remove the single-use code before rendering links or loading other resources.
history.replaceState(null, '', location.pathname)

onMounted(async () => {
  try {
    await api('/plugins/oauth/callback', { method: 'POST', body: JSON.stringify(parameters) })
    completed.value = true
  }
  catch (e) {
    error.value = (e as Error).message
  }
})
</script>

<template>
  <OAuthStatusCard
    :embedded="props.embedded"
    :state="cardState"
    :title="cardTitle"
    :message="message"
    :error="error"
  />
</template>
