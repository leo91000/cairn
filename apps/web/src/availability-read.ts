// This account-shell GET may be withdrawn before pagehide. Keep it independent
// of the provisional document loader while retaining its original native result.
export class AvailabilityDeparture extends Error {
  constructor() {
    super('Availability read withdrawn by this document')
    this.name = 'AvailabilityDeparture'
  }
}

export function readAvailability(signal: AbortSignal) {
  const ownerWindow = window
  const ownerDocument = document
  const nativeFetch = ownerWindow.fetch
  const nativeJson = ownerWindow.Response.prototype.json
  const nativeAbort = ownerWindow.AbortController.prototype.abort
  const controller = new ownerWindow.AbortController()
  let listening = false
  let settled = false
  let consumed = false
  let insideAbort: { owner: 'departure' | 'caller', reason: unknown } | undefined
  let proof: NonNullable<typeof insideAbort> | undefined

  const callerReason = () => signal.reason ?? new DOMException('Availability caller withdrawn', 'AbortError')
  const removeListener = () => {
    if (listening) {
      ownerWindow.removeEventListener('beforeunload', beforeUnload)
      listening = false
    }
  }

  const cleanup = () => {
    removeListener()
    signal.removeEventListener('abort', callerAbort)
    controller.signal.removeEventListener('abort', nativeSignalAborted)
  }

  const finish = () => {
    settled = true
    cleanup()
  }

  function nativeSignalAborted(event: Event) {
    if (insideAbort && event.type === 'abort' && event.isTrusted
      && event instanceof ownerWindow.Event && event.target === controller.signal && event.currentTarget === controller.signal
      && controller.signal.aborted && controller.signal.reason === insideAbort.reason) {
      // This records provenance only. The original fetch/json promise, never
      // this signal event, must supply the terminal result.
      proof = insideAbort
    }
  }

  function abort(owner: 'departure' | 'caller') {
    if (settled || controller.signal.aborted)
      return
    removeListener()
    insideAbort = { owner, reason: owner === 'departure' ? {} : callerReason() }
    try {
      nativeAbort.call(controller, insideAbort.reason)
    }
    finally {
      insideAbort = undefined
    }
  }

  function callerAbort() {
    abort('caller')
  }

  function beforeUnload(event: BeforeUnloadEvent) {
    const ownIntent = event.isTrusted && event.type === 'beforeunload'
      && typeof ownerWindow.BeforeUnloadEvent === 'function' && event instanceof ownerWindow.BeforeUnloadEvent
      && event.target === ownerDocument && event.currentTarget === ownerWindow
      && ownerWindow.document === ownerDocument && ownerDocument.defaultView === ownerWindow
      && ownerWindow.top === ownerWindow
    if (ownIntent && !settled)
      abort('departure')
  }

  const departureCause = (cause: unknown, httpFailure = false) => {
    const nativeAbortError = cause instanceof ownerWindow.DOMException && cause.name === 'AbortError'
      && cause.code === ownerWindow.DOMException.ABORT_ERR
    // Keep the fetch operation closed: only the native GET and native json()
    // are used; no raw Response, stream, clone or supplied body is exposed.
    // Network failures and native JSON parse errors must retain their causes.
    if (!httpFailure && proof?.owner === 'departure' && controller.signal.reason === proof.reason
      && (cause === proof.reason || nativeAbortError)) {
      return new AvailabilityDeparture()
    }
    return cause
  }

  if (signal.aborted) {
    return { response: Promise.reject(callerReason()), dispose() {} }
  }

  controller.signal.addEventListener('abort', nativeSignalAborted)
  signal.addEventListener('abort', callerAbort, { once: true })
  ownerWindow.addEventListener('beforeunload', beforeUnload)
  listening = true
  let response: Promise<{
    status: number
    ok: boolean
    json: () => Promise<unknown>
  }>
  try {
    const original = nativeFetch.call(ownerWindow, '/api/installations', {
      credentials: 'same-origin',
      keepalive: true,
      signal: controller.signal,
    })
    response = original.then((value) => {
      let bodyResult: Promise<unknown> | undefined
      const consume = () => {
        if (bodyResult)
          return bodyResult
        try {
          const originalBody = nativeJson.call(value)
          bodyResult = originalBody.then((data) => {
            finish()
            return data
          }, (cause: unknown) => {
            const result = departureCause(cause, !value.ok)
            finish()
            throw result
          })
        }
        catch (cause) {
          finish()
          bodyResult = Promise.reject(cause)
        }
        // A retiring caller may never request JSON. Observe this owned result
        // without replacing it: json() still returns the same success/failure.
        void bodyResult.catch(() => {})
        return bodyResult
      }

      if (value.ok)
        consume()
      return {
        status: value.status,
        ok: value.ok,
        json() {
          if (consumed)
            return Promise.reject(new TypeError('Availability body already consumed'))
          consumed = true
          return consume()
        },
      }
    }, (cause: unknown) => {
      const result = departureCause(cause)
      finish()
      throw result
    })
  }
  catch (cause) {
    finish()
    response = Promise.reject(cause)
  }

  return {
    response,
    dispose() {
      try {
        abort('caller')
      }
      finally {
        cleanup()
      }
    },
  }
}
