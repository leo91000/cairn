<script setup lang="ts">
import type { PluginOAuthProvider, PluginOAuthStatus } from '../plugins'
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { api } from '../api'
import PluginAuthorization from './PluginAuthorization.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{
  path: string
  provider: PluginOAuthProvider
  enabled: boolean
  blocked?: boolean
  setupRevision?: string
}>()
const emit = defineEmits<{
  changed: []
  state: [value: { busy: boolean, pending: boolean }]
}>()
const authorization = ref<PluginOAuthStatus>()
const error = ref('')
const loading = ref(false)
const activity = ref({ busy: false, pending: false })
const directOrigin = ref('')
let active = true

function state(value = activity.value) {
  activity.value = value
  emit('state', { busy: loading.value || value.busy, pending: value.pending })
}

async function load() {
  if (loading.value || activity.value.busy || activity.value.pending)
    return
  loading.value = true
  state()
  error.value = ''
  try {
    const result = await api<PluginOAuthStatus>(props.path)
    if (active)
      authorization.value = result
  }
  catch (e) {
    if (active)
      error.value = (e as Error).message
  }
  finally {
    loading.value = false
    if (active)
      state()
  }
}

onMounted(load)
onBeforeUnmount(() => {
  active = false
  emit('state', { busy: false, pending: false })
})
</script>

<template>
  <section class="grid min-w-0 gap-3 border-t border-line pt-3" :aria-label="provider.title">
    <h4 class="text-sm! font-semibold">
      {{ provider.title }}
    </h4>
    <p class="text-xs text-muted">
      Shared resource authorization. This does not grant agents access.
    </p>
    <p v-if="blocked" class="text-xs text-muted">
      Finish the other authorization on this resource first.
    </p>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <UiButton size="small" :disabled="loading || blocked || activity.busy || activity.pending" @click="load">
      {{ loading ? 'Checking authorization…' : 'Refresh authorization' }}
    </UiButton>
    <template v-if="authorization">
      <label class="text-xs">Direct HTTPS origin (optional)<input
        v-model="directOrigin"
        type="url"
        placeholder="Use instance default"
        :disabled="!enabled || loading || blocked || activity.busy || activity.pending"
      ><small>Use the origin you expose yourself. Leave empty for this instance’s configured callback (Beacon by default).</small></label>
      <PluginAuthorization
        :path="path"
        :title="provider.title"
        :scopes="provider.scopes"
        :initial="authorization"
        :enabled="enabled && !loading"
        :blocked="blocked"
        :setup-revision="setupRevision"
        :direct-origin="directOrigin"
        @state="state"
        @changed="emit('changed')"
      />
    </template>
  </section>
</template>
