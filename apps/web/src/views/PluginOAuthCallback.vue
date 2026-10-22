<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { accountApi, api, state } from '../api'
import UiAlert from '../components/UiAlert.vue'

const props = defineProps<{ central?: boolean }>()
const error = ref('')
const completed = ref(false)
const parameters = Object.fromEntries(new URLSearchParams(location.search))
// Remove the single-use code before rendering links or loading other resources.
history.replaceState(null, '', location.pathname)

onMounted(async () => {
  try {
    if (props.central) {
      const session = await accountApi('/session')
      if (!session.authenticated || !session.csrf)
        throw new Error('Sign in to your Cairn account, then restart the plugin sign-in from its account editor.')
      state.csrf = session.csrf
    }

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
