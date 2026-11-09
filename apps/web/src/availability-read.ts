// Only the account shell's readonly availability GET uses this transport. A
// native departure can cancel fetch before pagehide and incorrectly mark an
// otherwise healthy installation offline.
export class AvailabilityDeparture extends Error {
  constructor() {
    super('Availability read withdrawn by this document')
    this.name = 'AvailabilityDeparture'
  }
}

interface AvailabilityResponse {
  status: number
  ok: boolean
  json: () => Promise<unknown>
}

export function readAvailability(signal: AbortSignal) {
  const ownerWindow = window
  const ownerDocument = document
  const request = new XMLHttpRequest()
  const nativeAbort = ownerWindow.XMLHttpRequest.prototype.abort
  let listening = false
  let delivered = false
  let consumed = false
  let insideAbort: 'departure' | 'caller' | undefined
  let terminal: { text: string } | { error: unknown } | undefined
  let body: { resolve: (text: string) => void, reject: (cause: unknown) => void } | undefined
  let resolveResponse!: (response: AvailabilityResponse) => void
  let rejectResponse!: (cause: unknown) => void
  const response = new Promise<AvailabilityResponse>((resolve, reject) => {
    resolveResponse = resolve
    rejectResponse = reject
  })

  const removeListener = () => {
    if (listening) {
      ownerWindow.removeEventListener('beforeunload', beforeUnload)
      listening = false
    }
  }
  const headers = () => {
    if (delivered || !request.status)
      return
    delivered = true
    resolveResponse({
      status: request.status,
      ok: request.status >= 200 && request.status < 300,
      json() {
        if (consumed)
          return Promise.reject(new TypeError('Availability body already consumed'))
        consumed = true
        const text = terminal
          ? 'text' in terminal ? Promise.resolve(terminal.text) : Promise.reject(terminal.error)
          : new Promise<string>((resolve, reject) => body = { resolve, reject })
        return text.then(value => JSON.parse(value) as unknown)
      },
    })
  }
  const finish = (outcome: NonNullable<typeof terminal>) => {
    if (terminal)
      return
    terminal = outcome
    removeListener()
    signal.removeEventListener('abort', callerAbort)
    if ('error' in outcome) {
      if (!delivered)
        rejectResponse(outcome.error)
      body?.reject(outcome.error)
    }
    else {
      headers()
      body?.resolve(outcome.text)
    }
    request.onreadystatechange = null
    request.onload = null
    request.onerror = null
    request.ontimeout = null
    request.onabort = null
  }
  const callerReason = () => signal.reason ?? new DOMException('Availability caller withdrawn', 'AbortError')
  const abort = (owner: 'departure' | 'caller') => {
    if (terminal)
      return
    removeListener()
    insideAbort = owner
    try {
      nativeAbort.call(request)
    }
    catch (cause) {
      finish({ error: cause })
    }
    finally {
      insideAbort = undefined
    }
    // Caller disposal must release its JS resources even if the UA has already
    // stopped this request. This is not evidence of a native departure terminal.
    if (owner === 'caller' && !terminal)
      finish({ error: callerReason() })
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
    if (!ownIntent || terminal)
      return
    removeListener()
    abort('departure')
  }
  const nativeEvent = (event: ProgressEvent, type: string) => event.type === type && event.isTrusted
    && typeof ownerWindow.ProgressEvent === 'function' && event instanceof ownerWindow.ProgressEvent
    && event.target === request && event.currentTarget === request
  request.onreadystatechange = () => {
    if (request.readyState >= 2)
      headers()
  }
  request.onload = (event) => {
    if (nativeEvent(event, 'load'))
      finish(request.status ? { text: request.responseText } : { error: new TypeError('Installation status unavailable') })
  }
  request.onerror = (event) => {
    if (nativeEvent(event, 'error'))
      finish({ error: new TypeError('Installation status unavailable') })
  }
  request.ontimeout = (event) => {
    if (nativeEvent(event, 'timeout'))
      finish({ error: new TypeError('Installation status timed out') })
  }
  request.onabort = (event) => {
    if (nativeEvent(event, 'abort')) {
      const cause = insideAbort === 'departure'
        ? new AvailabilityDeparture()
        : insideAbort === 'caller' ? callerReason() : new DOMException('Installation status read aborted', 'AbortError')
      finish({ error: cause })
    }
  }
  try {
    if (signal.aborted) {
      finish({ error: callerReason() })
    }
    else {
      request.open('GET', '/api/installations')
      ownerWindow.addEventListener('beforeunload', beforeUnload)
      listening = true
      signal.addEventListener('abort', callerAbort, { once: true })
      request.send()
    }
  }
  catch (cause) {
    finish({ error: cause })
  }

  return {
    response,
    dispose: callerAbort,
  }
}
