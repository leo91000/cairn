import type { RunListItem } from '../shared/contracts'
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { api, state } from './api'

// Recent mission runs shared by the Fil, the palette and the shell badges: one slow poll while any
// view needs it, because the server rate-limits each client. Actions refresh it immediately.
const runs = ref<RunListItem[]>([])
let users = 0
let fetchedAt = 0
let timer: ReturnType<typeof setInterval> | undefined
watch(() => state.authenticated, (signedIn) => {
  if (!signedIn) {
    runs.value = []
    fetchedAt = 0
  }
})

// Only while signed in: a request racing sign-in or sign-out must not reset the session.
export async function refreshMissionRuns() {
  if (!state.authenticated || state.signingOut || state.redirecting)
    return
  fetchedAt = Date.now()
  try {
    runs.value = await api<RunListItem[]>('/runs?offset=0&limit=50')
  }
  catch {
    // Keep the last known runs; the Fil stays usable while the server reconnects.
  }
}

/** Views showing missions load them at once; the shell's badges wait so pages load first. */
export function useMissionRuns({ eager = true } = {}) {
  let first: ReturnType<typeof setTimeout> | undefined
  const load = () => {
    if (Date.now() - fetchedAt > 5000)
      void refreshMissionRuns()
  }
  const start = () => {
    clearTimeout(first)
    first = setTimeout(load, eager ? 0 : 3000)
  }
  const stop = watch(() => state.authenticated, signedIn => signedIn && start())
  onMounted(() => {
    start()
    if (users++ === 0) {
      timer = setInterval(() => {
        if (!document.hidden)
          void refreshMissionRuns()
      }, 30000)
    }
  })
  onBeforeUnmount(() => {
    clearTimeout(first)
    stop()
    if (--users === 0)
      clearInterval(timer)
  })
  return runs
}
