import type { WebPerformanceReport } from './client-performance'

interface UploadContext {
  accountId: string
  csrf: string
  foreground: boolean
  enabled: boolean
}

export interface UploadEnvironment {
  now: () => number
  schedule: (callback: () => void, milliseconds: number) => () => void
  report: () => WebPerformanceReport | null
  enabled: () => boolean
  send: (report: WebPerformanceReport, csrf: string, signal: AbortSignal) => Promise<boolean>
}

/** One bounded attempt per minute, including failures, toggles and account changes. */
export function performanceUploader(environment: UploadEnvironment) {
  let context: UploadContext = {
    accountId: '',
    csrf: '',
    foreground: false,
    enabled: false,
  }
  let stopped = false
  let generation = 0
  let eligibleAt = environment.now() + 10_000
  let accepted: string | undefined
  let cancelTimer: (() => void) | undefined
  let request: AbortController | undefined

  function eligible() {
    return !stopped && context.foreground && context.enabled && !!context.accountId && !!context.csrf && environment.enabled()
  }

  function schedule() {
    cancelTimer?.()
    cancelTimer = undefined
    if (eligible() && !request)
      cancelTimer = environment.schedule(() => { void upload() }, Math.max(0, eligibleAt - environment.now()))
  }

  async function upload() {
    cancelTimer = undefined
    if (!eligible())
      return
    eligibleAt = environment.now() + 60_000
    const report = environment.report()
    const identity = report && `${report.measurements.windowId}:${report.measurements.sequence}`
    if (!report || report.measurements.sequence <= 0 || identity === accepted) {
      schedule()
      return
    }

    const current = generation
    const controller = new AbortController()
    request = controller
    const cancelTimeout = environment.schedule(() => controller.abort(), 5_000)
    try {
      const success = await environment.send(report, context.csrf, controller.signal)
      if (success && !controller.signal.aborted && generation === current && eligible())
        accepted = identity || undefined
    }
    catch { /* Diagnostics must never interfere with account or installation use. */ }
    finally {
      cancelTimeout()
      if (request === controller)
        request = undefined
      schedule()
    }
  }

  return {
    update(next: UploadContext) {
      const wasEligible = eligible()
      const accountChanged = next.accountId !== context.accountId
      const changed = next.accountId !== context.accountId || next.csrf !== context.csrf || next.foreground !== context.foreground || next.enabled !== context.enabled
      if (changed) {
        generation++
        request?.abort()
      }

      if (accountChanged)
        accepted = undefined
      context = { ...next }
      if (eligible() && (!wasEligible || accountChanged))
        eligibleAt = Math.max(eligibleAt, environment.now() + 10_000)
      schedule()
    },
    stop() {
      stopped = true
      generation++
      cancelTimer?.()
      request?.abort()
    },
  }
}
