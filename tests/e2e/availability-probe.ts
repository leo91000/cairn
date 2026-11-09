import type { Page } from '@playwright/test'

export interface AvailabilityReadEvent {
  sequence: number
  kind: 'start' | 'fetch-fulfilled' | 'headers' | 'body' | 'body-start' | 'rejected' | 'body-rejected' | 'abort' | 'abort-call-enter' | 'abort-call-return' | 'pagehide' | 'pageshow'
  document: string
  read: number
  status?: number
  ok?: boolean
  aborted: boolean
  afterPagehide: boolean
  persisted?: boolean
  trusted?: boolean
  exactTarget?: boolean
  insideAbortCall?: boolean
  exactBody?: boolean
  source?: 'signal'
  errorName?: string
  reasonMatches?: boolean
}

declare global {
  interface Window {
    cairnAvailabilityDocument: string
    cairnExpectedAvailability?: unknown
    cairnAvailabilityRecord: (event: AvailabilityReadEvent) => Promise<void>
  }
}

export async function installAvailabilityProbe(page: Page, recordRead: (event: AvailabilityReadEvent) => void) {
  await page.exposeFunction('cairnAvailabilityRecord', recordRead)
  await page.addInitScript(() => {
    const documentId = crypto.randomUUID()
    window.cairnAvailabilityDocument = documentId
    let pageHidden = false
    let sequence = 0
    let eventSequence = 0
    const record = (event: Omit<AvailabilityReadEvent, 'sequence'>) => {
      const entry = { ...event, sequence: ++eventSequence }
      const journal = JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
      journal.push(entry)
      sessionStorage.setItem('cairn-e2e-availability-journal', JSON.stringify(journal))
      void window.cairnAvailabilityRecord(entry)
    }

    for (const kind of ['pagehide', 'pageshow'] as const) {
      window.addEventListener(kind, (event) => {
        pageHidden = kind === 'pagehide'
        record({
          kind,
          document: documentId,
          read: 0,
          aborted: false,
          afterPagehide: pageHidden,
          persisted: event.persisted,
          trusted: event.isTrusted,
        })
      })
    }

    interface Metadata {
      read: number
      signal?: AbortSignal | null
      insideAbortCall: boolean
      nativeSignal: boolean
    }

    const signals = new WeakMap<AbortSignal, Metadata>()
    const responses = new WeakMap<Response, Metadata>()
    const fetch = window.fetch
    const json = Response.prototype.json
    const abort = AbortController.prototype.abort
    const recordRead = (meta: Metadata, kind: AvailabilityReadEvent['kind'], detail: Partial<AvailabilityReadEvent> = {}) => record({
      kind,
      document: documentId,
      read: meta.read,
      aborted: false,
      afterPagehide: pageHidden,
      source: 'signal',
      ...detail,
    })
    const rejected = (meta: Metadata, kind: 'rejected' | 'body-rejected', cause: unknown) => {
      const reasonMatches = Boolean(meta.signal?.aborted && cause === meta.signal.reason)
      const nativeAbortError = cause instanceof DOMException && cause.name === 'AbortError' && cause.code === DOMException.ABORT_ERR
      recordRead(meta, kind, {
        aborted: meta.nativeSignal && (reasonMatches || nativeAbortError),
        errorName: cause instanceof Error ? cause.name : typeof cause,
        reasonMatches,
      })
    }

    window.fetch = function (this: Window, input: RequestInfo | URL, init?: RequestInit) {
      const tracked = String(input) === '/api/installations' && (!init?.method || init.method === 'GET')
      if (!tracked)
        return fetch.call(this, input, init)
      const meta: Metadata = {
        read: ++sequence,
        signal: init?.signal,
        insideAbortCall: false,
        nativeSignal: false,
      }
      if (meta.signal) {
        signals.set(meta.signal, meta)
        meta.signal.addEventListener('abort', (event) => {
          const native = event.type === 'abort' && event.isTrusted && event instanceof Event
            && event.target === meta.signal && event.currentTarget === meta.signal && meta.signal!.aborted
          meta.nativeSignal = native && meta.insideAbortCall
          recordRead(meta, 'abort', { trusted: event.isTrusted, exactTarget: native, insideAbortCall: meta.insideAbortCall })
        }, { once: true })
      }

      recordRead(meta, 'start')
      let original: ReturnType<typeof fetch>
      try {
        original = fetch.call(this, input, init)
      }
      catch (cause) {
        rejected(meta, 'rejected', cause)
        throw cause
      }

      // Observe without replacing the original promise or cloning its Response.
      void original.then((response) => {
        responses.set(response, meta)
        recordRead(meta, 'fetch-fulfilled', { status: response.status, ok: response.ok })
        recordRead(meta, 'headers', { status: response.status })
      }, cause => rejected(meta, 'rejected', cause))
      return original
    }

    Response.prototype.json = function (this: Response) {
      const meta = responses.get(this)
      if (!meta)
        return json.call(this)
      recordRead(meta, 'body-start')
      let original: ReturnType<typeof json>
      try {
        original = json.call(this)
      }
      catch (cause) {
        rejected(meta, 'body-rejected', cause)
        throw cause
      }

      void original.then((value: unknown) => {
        recordRead(meta, 'body', {
          status: this.status,
          exactBody: window.cairnExpectedAvailability !== undefined
            && JSON.stringify(value) === JSON.stringify(window.cairnExpectedAvailability),
        })
      }, cause => rejected(meta, 'body-rejected', cause))
      return original
    }

    AbortController.prototype.abort = function (this: AbortController, reason?: unknown) {
      const meta = signals.get(this.signal)
      if (!meta)
        return abort.call(this, reason)
      meta.insideAbortCall = true
      recordRead(meta, 'abort-call-enter', { insideAbortCall: true })
      try {
        return abort.call(this, reason)
      }
      finally {
        meta.insideAbortCall = false
        recordRead(meta, 'abort-call-return', { insideAbortCall: false })
      }
    }
  })
}

// Both the request and its exact body must follow this document's boundary.
// A body completed before departure, or from an older pending read, is stale.
export function availabilityBodiesAfter(reads: readonly AvailabilityReadEvent[], document: string, sequence: number) {
  return reads.filter((event) => {
    if (event.document !== document || event.kind !== 'body' || !event.exactBody || event.aborted || event.afterPagehide)
      return false

    const starts = reads.filter(start => start.document === document && start.read === event.read && start.kind === 'start')
    return starts.length === 1 && starts[0]!.sequence > sequence && event.sequence > starts[0]!.sequence
  })
}

// Validate native fetch and optional JSON phases separately. A deliberately
// unread HTTP failure still requires owned disposal; its wire closure must be
// proved independently by the response gate, never inferred from this journal.
export function incompleteAvailabilityReads(reads: readonly AvailabilityReadEvent[]) {
  const kinds = new Set(['start', 'fetch-fulfilled', 'headers', 'body-start', 'body', 'rejected', 'body-rejected', 'abort-call-enter', 'abort', 'abort-call-return'])
  const identities = new Map<string, AvailabilityReadEvent[]>()
  for (const event of reads) {
    if (event.kind === 'pagehide' || event.kind === 'pageshow')
      continue
    const identity = `${event.document}:${event.read}`
    identities.set(identity, [...(identities.get(identity) ?? []), event])
  }

  return [...identities.values()].filter((events) => {
    if (events.some(event => !kinds.has(event.kind)) || new Set(events.map(event => event.sequence)).size !== events.length)
      return true
    const starts = events.filter(event => event.kind === 'start')
    const fulfilled = events.filter(event => event.kind === 'fetch-fulfilled')
    const rejected = events.filter(event => event.kind === 'rejected')
    const headers = events.filter(event => event.kind === 'headers')
    const bodies = events.filter(event => event.kind === 'body-start')
    const outcomes = events.filter(event => event.kind === 'body' || event.kind === 'body-rejected')
    if (starts.length !== 1 || fulfilled.length + rejected.length !== 1 || bodies.length > 1)
      return true
    const start = starts[0]!
    if (events.some(event => event !== start && event.sequence <= start.sequence))
      return true
    const enters = events.filter(event => event.kind === 'abort-call-enter')
    const aborts = events.filter(event => event.kind === 'abort')
    const returns = events.filter(event => event.kind === 'abort-call-return')
    const nativeAbort = enters.length === 1 && aborts.length === 1 && returns.length === 1
      && enters[0]!.sequence < aborts[0]!.sequence && aborts[0]!.sequence < returns[0]!.sequence
      && aborts[0]!.trusted && aborts[0]!.exactTarget && aborts[0]!.insideAbortCall
    if (enters.length !== returns.length || enters.length !== aborts.length || enters.length > 1
      || (enters.length === 1 && enters[0]!.sequence >= returns[0]!.sequence)) {
      return true
    }

    if (aborts.length === 1) {
      const signal = aborts[0]!
      const inside = enters[0]!.sequence < signal.sequence && signal.sequence < returns[0]!.sequence
      if (!signal.trusted || !signal.exactTarget || signal.insideAbortCall !== inside
        || enters[0]!.insideAbortCall !== true || returns[0]!.insideAbortCall !== false) {
        return true
      }
    }

    if ([...rejected, ...outcomes].some(event => event.aborted && (!nativeAbort || event.sequence <= returns[0]!.sequence)))
      return true
    if (rejected.length)
      return headers.length !== 0 || bodies.length !== 0 || outcomes.length !== 0
    const response = fulfilled[0]!
    if (headers.length !== 1 || headers[0]!.sequence <= response.sequence
      || response.status === undefined || headers[0]!.status !== response.status
      || response.ok !== (response.status >= 200 && response.status < 300)) {
      return true
    }

    if (!bodies.length)
      return response.ok || outcomes.length !== 0 || !nativeAbort
    return outcomes.length !== 1 || bodies[0]!.sequence <= headers[0]!.sequence || outcomes[0]!.sequence <= bodies[0]!.sequence
  }).map(events => events.find(event => event.kind === 'start') ?? events[0]!)
}
