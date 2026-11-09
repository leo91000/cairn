import type { Page } from '@playwright/test'

export interface AvailabilityReadEvent {
  sequence: number
  kind: 'start' | 'headers' | 'body' | 'body-start' | 'rejected' | 'body-rejected' | 'abort' | 'abort-call-enter' | 'abort-call-return' | 'pagehide' | 'pageshow'
  document: string
  read: number
  status?: number
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
        record({ kind, document: documentId, read: 0, aborted: false, afterPagehide: pageHidden, persisted: event.persisted, trusted: event.isTrusted })
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
      const meta: Metadata = { read: ++sequence, signal: init?.signal, insideAbortCall: false, nativeSignal: false }
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

// A final journal must account for every producer, including retired documents.
// A known HTTP status alone does not establish native request/body settlement.
export function incompleteAvailabilityReads(reads: readonly AvailabilityReadEvent[]) {
  const starts = reads.filter(event => event.kind === 'start')
  const terminals = reads.filter(event => ['body', 'body-rejected', 'rejected'].includes(event.kind))
  const identities = new Map<string, AvailabilityReadEvent>()
  for (const event of reads) {
    if (event.kind === 'pagehide' || event.kind === 'pageshow')
      continue
    const identity = `${event.document}:${event.read}`
    if (!identities.has(identity))
      identities.set(identity, event)
  }

  return [...identities.values()].filter((read) => {
    const sameRead = (event: AvailabilityReadEvent) => event.document === read.document && event.read === read.read
    return starts.filter(sameRead).length !== 1 || terminals.filter(sameRead).length !== 1
  })
}
