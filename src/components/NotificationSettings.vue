<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { accountApi, state } from '../api'
import { BellRing } from '../icons'
import AccountConfirmation from './AccountConfirmation.vue'
import Icon from './Icon.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const deviceKey = `leo-push-device:${state.accountId}`
const signedIn = !!state.accountId
const supported = 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window && window.isSecureContext
const enabled = ref(false)
const busy = ref(false)
const loading = ref(true)
const error = ref('')
const denied = ref(supported && Notification.permission === 'denied')
const confirmingIdentity = ref(false)
const ios = /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)
const installed = window.matchMedia('(display-mode: standalone)').matches || (navigator as Navigator & { standalone?: boolean }).standalone
let registration: ServiceWorkerRegistration | undefined
onMounted(async () => {
  try {
    if (!supported || !signedIn)
      return
    registration = await navigator.serviceWorker.getRegistration()
    const subscription = await registration?.pushManager.getSubscription()
    const id = localStorage.getItem(deviceKey)
    enabled.value = !!subscription && !!id && (await accountApi(`/notifications/subscriptions/${id}`)).registered
  }
  catch (e) { error.value = (e as Error).message }
  finally { loading.value = false }
})

async function toggle() {
  if (busy.value)
    return
  busy.value = true
  error.value = ''
  try {
    // Permission must be requested from the click itself (including on iOS).
    if (!enabled.value && await Notification.requestPermission() !== 'granted') {
      denied.value = Notification.permission === 'denied'
      return
    }

    registration ??= await navigator.serviceWorker.register('/sw.js')
    await navigator.serviceWorker.ready
    let subscription = await registration.pushManager.getSubscription()
    if (enabled.value) {
      const id = localStorage.getItem(deviceKey)
      if (id)
        await accountApi(`/notifications/subscriptions/${id}`, { method: 'DELETE' })
      await subscription?.unsubscribe()
      localStorage.removeItem(deviceKey)
      enabled.value = false
      return
    }

    const { publicKey } = await accountApi<{ publicKey: string }>('/notifications')
    const key = Uint8Array.from(atob(publicKey.replace(/-/g, '+').replace(/_/g, '/')), c => c.charCodeAt(0))
    const currentKey = subscription?.options.applicationServerKey
    const currentKeyBytes = currentKey ? new Uint8Array(currentKey) : undefined
    const usesOfficialKey = !!currentKeyBytes
      && currentKeyBytes.length === key.length
      && currentKeyBytes.every((byte, index) => byte === key[index])

    if (subscription && !usesOfficialKey) {
      await subscription.unsubscribe()
      subscription = null
    }

    subscription ??= await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key })
    const { id } = await accountApi<{ id: string }>('/notifications/subscriptions', { method: 'POST', body: JSON.stringify(subscription.toJSON()) })
    localStorage.setItem(deviceKey, id)
    enabled.value = true
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
</script>

<template>
  <AccountConfirmation
    v-if="confirmingIdentity"
    title="Confirm identity before enabling notifications"
    return-label="Back to notifications"
    @close="confirmingIdentity = false"
    @confirmed="confirmingIdentity = false; error = ''"
  />
  <div v-show="!confirmingIdentity" class="space-y-3">
    <div class="flex items-start gap-3">
      <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-accent/10 text-accent"><Icon :name="BellRing" :size="20" /></span>
      <div>
        <h3 class="text-sm font-semibold">
          Question notifications
        </h3>
        <p class="mt-1! mb-0! text-xs leading-relaxed text-muted">
          Know when your agent needs your input, across all your installations, even with the app closed. Question content stays private.
        </p>
      </div>
    </div>
    <p v-if="!signedIn" class="text-xs text-muted">
      Sign in to your Leo account to manage push notifications on this device.
    </p>
    <p v-else-if="ios && !installed" class="text-xs text-muted">
      On iPhone or iPad, add Leo to your Home Screen from Safari’s Share menu, then enable notifications in that app.
    </p>
    <p v-else-if="!supported" class="text-xs text-muted">
      This browser doesn’t support push notifications. Use a browser with Web Push on HTTPS.
    </p>
    <p v-else-if="denied" class="text-xs text-muted">
      Notifications are blocked. Allow them in this site’s browser settings, then reopen this panel.
    </p>
    <div v-else class="flex items-center gap-3">
      <UiButton
        v-if="!enabled"
        size="small"
        :disabled="busy || loading"
        @click="confirmingIdentity = true"
      >
        Confirm identity
      </UiButton>
      <UiButton
        size="small"
        :variant="enabled ? 'default' : 'primary'"
        :disabled="busy || loading"
        @click="toggle"
      >
        {{ busy ? 'Updating…' : enabled ? 'Disable on this device' : 'Enable on this device' }}
      </UiButton><span v-if="enabled" class="text-[11px] text-accent" role="status">Notifications on</span>
    </div>
    <UiAlert v-if="error">
      {{ error }}
    </UiAlert>
  </div>
</template>
