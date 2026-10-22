<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api } from '../api'
import UiAlert from '../components/UiAlert.vue'

const error = ref('')
const completed = ref(false)
const parameters = Object.fromEntries(new URLSearchParams(location.search))
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
  <UiAlert v-if="error">
    {{ error }}
  </UiAlert>
  <p v-else role="status">
    {{ completed ? 'Signed in. Return to the account editor in Cairn and save the account.' : 'Completing sign-in…' }}
  </p>
</template>
