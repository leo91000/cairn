<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { api } from '../api'
import UiAlert from '../components/UiAlert.vue'

const router = useRouter()
const status = ref('Completing MCP sign-in…')
const error = ref('')
const parameters = Object.fromEntries(new URLSearchParams(location.search))

onMounted(async () => {
  // Remove the single-use code before further navigation or resource reads.
  history.replaceState(null, '', location.pathname)
  try {
    const completed = await api('/mcps/oauth/callback', {
      method: 'POST',
      body: JSON.stringify(parameters),
    })
    if (completed.result === 'native')
      status.value = 'Return to Cairn to finish connecting this MCP server.'
    else
      await router.replace({ path: '/mcps', query: { oauth: completed.result } })
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
    {{ status }}
  </p>
</template>
