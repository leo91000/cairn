<script setup lang="ts">
import type { PluginOAuthConsent, PluginOAuthStatus } from '../plugins'
import {
  computed,
  onBeforeUnmount,
  ref,
  watch,
} from 'vue'
import { api } from '../api'
import Modal from './Modal.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const props = defineProps<{
  path: string
  title: string
  scopes: string[]
  initial: PluginOAuthStatus
  enabled: boolean
}>()
const emit = defineEmits<{ changed: [] }>()
const authorization = ref(props.initial)
const selected = ref((props.initial.requestedScopes ?? props.initial.scopes ?? []).filter(scope => props.scopes.includes(scope)))
// Consent state and URL stay in this window's memory; credentials never reach it.
const consent = ref<PluginOAuthConsent>()
const busy = ref(false)
const error = ref('')
const notice = ref('')
const disconnecting = ref(false)
let active = true
let expiry: ReturnType<typeof setTimeout> | undefined
const connected = computed(() => authorization.value.status === 'authorized')
const reconnect = computed(() => authorization.value.status === 'reconnect')
watch(() => props.initial, value => authorization.value = value)
watch(() => props.scopes, scopes => selected.value = selected.value.filter(scope => scopes.includes(scope)))
watch(consent, (value) => {
  clearTimeout(expiry)
  if (value) {
    expiry = setTimeout(() => {
      consent.value = undefined
      notice.value = 'Authorization expired. Start again.'
    }, Math.max(0, value.expiresAt - Date.now()))
  }
})

async function action(operation: () => Promise<void>) {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  notice.value = ''
  try {
    await operation()
  }
  catch (e) {
    if (active)
      error.value = (e as Error).message
  }
  finally {
    busy.value = false
  }
}

async function start() {
  if (!props.enabled || !selected.value.length)
    return
  await action(async () => {
    const value = await api<PluginOAuthConsent>(props.path, { method: 'POST', body: JSON.stringify({ scopes: selected.value }) })
    const url = new URL(value.url)
    if (url.protocol !== 'https:' || url.username || url.password)
      throw new Error('The provider returned an invalid authorization link.')
    if (active)
      consent.value = value
  })
}

async function finish() {
  const pending = consent.value
  if (!pending)
    return
  await action(async () => {
    try {
      const result = await api<PluginOAuthStatus>(`${props.path}/finish`, { method: 'POST', body: JSON.stringify({ state: pending.state }) })
      if (!active)
        return
      if (result.status === 'pending') {
        notice.value = 'Finish authorizing in your browser.'
        return
      }

      consent.value = undefined
      if (result.status === 'authorized')
        authorization.value = result
      else
        notice.value = 'Authorization was declined.'
      emit('changed')
    }
    catch (e) {
      // An uncertain exchange is never retried automatically with a consumed code.
      consent.value = undefined
      throw e
    }
  })
}

async function disconnect() {
  await action(async () => {
    await api(props.path, { method: 'DELETE' })
    if (!active)
      return
    consent.value = undefined
    authorization.value = { status: 'disconnected' }
    disconnecting.value = false
    emit('changed')
  })
}

onBeforeUnmount(() => {
  active = false
  clearTimeout(expiry)
  consent.value = undefined
})
</script>

<template>
  <div class="grid min-w-0 gap-3">
    <p class="text-sm" role="status">
      {{ connected ? authorization.subject ? `Connected as ${authorization.subject}` : 'Connected' : reconnect ? 'Reconnect to continue' : 'Not connected' }}
    </p>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
    <p v-if="notice" class="text-sm text-muted" role="status">
      {{ notice }}
    </p>
    <p v-if="!enabled" class="text-xs text-muted">
      This resource is paused or unavailable. You can still disconnect your saved authorization.
    </p>
    <fieldset class="grid min-w-0 gap-2" :disabled="busy || !enabled || Boolean(consent)">
      <legend class="mb-2 text-xs font-semibold">
        Permissions to authorize
      </legend>
      <label v-for="scope in scopes" :key="scope" class="flex flex-row! items-center gap-2! break-words text-sm">
        <input
          v-model="selected"
          type="checkbox"
          class="w-auto!"
          :value="scope"
        >{{ scope }}
      </label>
    </fieldset>
    <p v-if="authorization.scopes?.length" class="break-words text-xs text-muted">
      Authorized: {{ authorization.scopes.join(', ') }}
    </p>
    <template v-if="consent">
      <a
        class="text-sm font-semibold text-accent underline"
        :href="consent.url"
        target="_blank"
        rel="noopener noreferrer"
      >Open authorization page</a>
      <p class="text-xs text-muted">
        Return here after authorizing. If you leave this page, start authorization again.
      </p>
      <UiButton :disabled="busy" @click="finish">
        Check authorization
      </UiButton>
    </template>
    <UiButton v-else :disabled="busy || !enabled || !selected.length" @click="start">
      {{ connected || reconnect ? 'Reconnect' : 'Connect' }}
    </UiButton>
    <UiButton v-if="consent || authorization.status !== 'disconnected'" :disabled="busy" @click="disconnecting = true">
      Disconnect
    </UiButton>
  </div>
  <Modal v-if="disconnecting" :title="`Disconnect ${title}?`" @close="!busy && (disconnecting = false)">
    <div class="grid gap-4 p-5">
      <p class="text-sm">
        Removes the saved authorization from Cairn and cancels pending sign-ins. To revoke access at the provider, use that service’s settings.
      </p>
      <UiAlert v-if="error">
        {{ error }}
      </UiAlert>
      <div class="flex justify-end gap-2">
        <UiButton :disabled="busy" @click="disconnecting = false">
          Cancel
        </UiButton>
        <UiButton :disabled="busy" @click="disconnect">
          Disconnect
        </UiButton>
      </div>
    </div>
  </Modal>
</template>
