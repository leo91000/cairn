import type { Run } from '../shared/contracts'
import {
  computed,
  onScopeDispose,
  ref,
  watch,
} from 'vue'

export function retryNotice(run: Run | null | undefined, at = Date.now()) {
  const retry = run?.retry
  if (run?.status !== 'queued' || !retry?.nextAttemptAt || run.accountRequired)
    return null
  const reason = run.accountWaitReason?.trim()
  if (reason && !reason.startsWith('Temporary agent error.'))
    return null
  const seconds = Math.max(0, Math.ceil((retry.nextAttemptAt - at) / 1000))
  const when = seconds ? `in ${seconds} seconds` : 'now'
  return `Temporary agent error. Retrying ${when} (attempt ${retry.attempt}/${retry.limit}).`
}

/** The countdown is local; no polling or live API requests are needed. */
export function useRetryNotice(run: () => Run | null | undefined) {
  const at = ref(Date.now())
  let timer: ReturnType<typeof setInterval> | undefined
  const stop = () => {
    if (timer)
      clearInterval(timer)
    timer = undefined
  }

  watch(() => run()?.status === 'queued' && !!run()?.retry?.nextAttemptAt, (waiting) => {
    stop()
    at.value = Date.now()
    if (waiting)
      timer = setInterval(() => at.value = Date.now(), 1000)
  }, { immediate: true })
  onScopeDispose(stop)
  return computed(() => retryNotice(run(), at.value))
}
