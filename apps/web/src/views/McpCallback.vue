<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { api } from '../api'
import OAuthStatusCard from '../components/OAuthStatusCard.vue'
import UiButton from '../components/UiButton.vue'

const router = useRouter()
const status = ref('Completing MCP sign-in…')
const completedNative = ref(false)
const error = ref('')
const parameters = Object.fromEntries(new URLSearchParams(location.search))
const cardState = computed(() => error.value ? 'error' : completedNative.value ? 'success' : 'loading')
const cardTitle = computed(() => error.value ? 'Connection failed' : completedNative.value ? 'MCP sign-in complete' : 'Finalizing MCP connection…')

onMounted(async () => {
  // Remove the single-use code before further navigation or resource reads.
  history.replaceState(null, '', location.pathname)
  try {
    const completed = await api('/mcps/oauth/callback', {
      method: 'POST',
      body: JSON.stringify(parameters),
    })
    if (completed.result === 'native') {
      completedNative.value = true
      status.value = 'Return to Cairn to finish connecting this MCP server.'
    }
    else {
      await router.replace({ path: '/mcps', query: { oauth: completed.result } })
    }
  }
  catch (e) {
    error.value = (e as Error).message
  }
})
</script>

<template>
  <OAuthStatusCard
    embedded
    :state="cardState"
    :title="cardTitle"
    :message="status"
    :error="error"
  >
    <template v-if="error" #actions>
      <UiButton variant="primary" class="w-full" @click="router.replace('/mcps')">
        Back to MCPs
      </UiButton>
    </template>
  </OAuthStatusCard>
</template>
