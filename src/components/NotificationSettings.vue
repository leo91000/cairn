<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { api } from '../api'
import { BellRing } from '../icons'
import Icon from './Icon.vue'
import UiAlert from './UiAlert.vue'
import UiButton from './UiButton.vue'

const supported = 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window && window.isSecureContext
const enabled = ref(false)
const busy = ref(false)
const loading = ref(true)
const error = ref('')
const denied = ref(supported && Notification.permission === 'denied')
const ios = /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)
const installed = window.matchMedia('(display-mode: standalone)').matches || (navigator as Navigator & { standalone?: boolean }).standalone
let registration: ServiceWorkerRegistration | undefined
onMounted(async () => {
  try {
    if (!supported)
      return
    registration = await navigator.serviceWorker.getRegistration()
    const subscription = await registration?.pushManager.getSubscription()
    const id = localStorage.getItem('leo-push-device')
    enabled.value = !!subscription && !!id && (await api(`/notifications/subscriptions/${id}`)).registered
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
      const id = localStorage.getItem('leo-push-device')
      if (id)
        await api(`/notifications/subscriptions/${id}`, { method: 'DELETE' })
      await subscription?.unsubscribe()
      localStorage.removeItem('leo-push-device')
      enabled.value = false
      return
    }

    const { publicKey } = await api<{ publicKey: string }>('/notifications')
    const key = Uint8Array.from(atob(publicKey.replace(/-/g, '+').replace(/_/g, '/')), c => c.charCodeAt(0))
    subscription ??= await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key })
    const { id } = await api<{ id: string }>('/notifications/subscriptions', { method: 'POST', body: JSON.stringify(subscription.toJSON()) })
    localStorage.setItem('leo-push-device', id)
    enabled.value = true
  }
  catch (e) { error.value = (e as Error).message }
  finally { busy.value = false }
}
</script>

<template>
  <div class="space-y-3">
    <div class="flex items-start gap-3">
      <span class="grid size-10 shrink-0 place-items-center rounded-xl bg-accent/10 text-accent"><Icon :name="BellRing" :size="20" /></span>
      <div>
        <h3 class="text-sm font-semibold">
          Question notifications
        </h3>
        <p class="mt-1! mb-0! text-xs leading-relaxed text-muted">
          Know when your agent needs your input, even with the app closed. Question content stays private.
        </p>
      </div>
    </div>
    <p v-if="ios && !installed" class="text-xs text-muted">
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
