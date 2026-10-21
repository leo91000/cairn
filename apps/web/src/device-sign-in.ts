import { computed, ref } from 'vue'
import { api, ApiError } from './api'

/** A plugin's device sign-in, as the installation shows it: the token never reaches the browser. */
export interface DeviceFlow {
  id: string
  pluginId: string
  state: 'pending' | 'completed' | 'failed' | 'expired'
  userCode: string
  verificationUri: string
  verificationUriComplete?: string | null
  expiresAt: number
  /** Why it failed, or the service's guidance. */
  message?: string | null
}

/** The page where the owner enters the code; some services prefill it. */
export function verificationLink(flow: Pick<DeviceFlow, 'verificationUri' | 'verificationUriComplete'>) {
  return flow.verificationUriComplete || flow.verificationUri
}

/**
 * What a form call or a save sends for the credential: a completed sign-in replaces a paste,
 * and neither when the saved credential stays.
 */
export function credentialInput(token: string, flow: Pick<DeviceFlow, 'id' | 'state'> | null | undefined) {
  if (flow?.state === 'completed')
    return { credentialFlow: flow.id }
  return token ? { token } : {}
}

/** Why a sign-in ended without a credential, or '' while it can still complete. */
export function signInProblem(flow: Pick<DeviceFlow, 'state' | 'message'> | null | undefined) {
  if (flow?.state === 'expired')
    return flow.message || 'This sign-in expired. Sign in again.'
  if (flow?.state === 'failed')
    return flow.message || 'The sign-in failed. Try again.'
  return ''
}

/**
 * Signs in to a plugin's service with a device code: start, poll every 2 s until it settles,
 * cancel. `onCompleted` runs once the installation holds the credential.
 */
export function useDeviceSignIn(plugin: () => string, onCompleted: (flow: DeviceFlow) => void, interval = 2000) {
  const flow = ref<DeviceFlow | null>(null)
  const starting = ref(false)
  const error = ref('')
  let timer: ReturnType<typeof setTimeout> | undefined

  const pending = computed(() => flow.value?.state === 'pending')
  const completed = computed(() => flow.value?.state === 'completed')

  function stop() {
    clearTimeout(timer)
    timer = undefined
  }

  function schedule() {
    stop()
    timer = setTimeout(() => void poll(), interval)
  }

  async function poll() {
    const current = flow.value
    if (current?.state !== 'pending')
      return

    let next: DeviceFlow
    try {
      next = await api<DeviceFlow>(`/plugins/${current.pluginId}/device-flow/${current.id}`)
    }
    catch (e) {
      // Cancelled or replaced while asking: that answer no longer matters.
      if (flow.value?.id !== current.id)
        return
      // A forgotten sign-in expired on the installation.
      if (e instanceof ApiError && e.status === 404) {
        flow.value = { ...current, state: 'expired', message: e.message }
        return
      }

      error.value = (e as Error).message
      schedule()
      return
    }

    if (flow.value?.id !== current.id)
      return
    error.value = ''
    flow.value = next
    if (next.state === 'pending')
      schedule()
    else if (next.state === 'completed')
      onCompleted(next)
  }

  async function start() {
    await cancel()
    starting.value = true
    error.value = ''
    try {
      flow.value = await api<DeviceFlow>(`/plugins/${plugin()}/device-flow`, { method: 'POST' })
      schedule()
    }
    catch (e) {
      error.value = (e as Error).message
    }
    finally {
      starting.value = false
    }
  }

  /** Forgets the sign-in, on the installation too unless a save consumed it. */
  async function cancel() {
    stop()
    const current = flow.value
    flow.value = null
    error.value = ''
    if (current && current.state !== 'expired')
      await api(`/plugins/${current.pluginId}/device-flow/${current.id}`, { method: 'DELETE' }).catch(() => {})
  }

  /** After a save consumed the sign-in: nothing is left to cancel. */
  function consumed() {
    stop()
    flow.value = null
    error.value = ''
  }

  return {
    flow,
    starting,
    error,
    pending,
    completed,
    start,
    cancel,
    consumed,
    stop,
  }
}
