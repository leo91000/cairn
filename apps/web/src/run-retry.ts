import type { Run } from '../../../packages/contracts/contracts'
import {
  computed,
  onScopeDispose,
  ref,
  watch,
} from 'vue'

export function retryState(run: Run | null | undefined, at = Date.now()) {
  const retry = run?.retry
  if (run?.status !== 'queued' || !retry?.nextAttemptAt || run.accountRequired)
    return null
  const reason = run.accountWaitReason?.trim()
  if (reason && !reason.startsWith('Temporary agent error.'))
    return null
  const seconds = Math.max(0, Math.ceil((retry.nextAttemptAt - at) / 1000))
  const when = seconds ? `in ${seconds} seconds` : 'now'
  return {
    attempt: retry.attempt,
    limit: retry.limit,
    seconds,
    cause: retry.cause.message,
    startup: !!retry.cause.startup,
    message: `Temporary agent error. Retrying ${when} (attempt ${retry.attempt}/${retry.limit}).`,
  }
}

export type RetryState = NonNullable<ReturnType<typeof retryState>>

export function retryNotice(run: Run | null | undefined, at = Date.now()) {
  return retryState(run, at)?.message ?? null
}

/** The countdown is local; no polling or live API requests are needed. */
export function useRetryState(run: () => Run | null | undefined) {
  const at = ref(Date.now())
  let timer: ReturnType<typeof setInterval> | undefined
  const stop = () => {
    if (timer)
      clearInterval(timer)
    timer = undefined
  }

  watch(() => !!retryState(run()), (waiting) => {
    stop()
    at.value = Date.now()
    if (waiting)
      timer = setInterval(() => at.value = Date.now(), 1000)
  }, { immediate: true })
  onScopeDispose(stop)
  return computed(() => retryState(run(), at.value))
}
