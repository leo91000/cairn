import type { Page } from '@playwright/test'

export interface AvailabilityReadEvent {
  sequence: number
  kind: 'start' | 'headers' | 'body' | 'rejected' | 'body-rejected' | 'abort' | 'abort-call-enter' | 'abort-call-return' | 'pagehide' | 'pageshow'
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
      // Persist metadata synchronously before the departing context can retire.
      // The live binding alone may lose its last messages during replacement.
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

    const metadata = new WeakMap<XMLHttpRequest, {
      read: number
      started: boolean
      headers: boolean
      insideAbortCall: boolean
      aborted: boolean
    }>()
    const open = XMLHttpRequest.prototype.open
    const send = XMLHttpRequest.prototype.send
    const abort = XMLHttpRequest.prototype.abort
    XMLHttpRequest.prototype.open = function (this: XMLHttpRequest, method: string, url: string | URL, async: boolean = true, username?: string | null, password?: string | null) {
      if (method === 'GET' && String(url) === '/api/installations') {
        const meta = {
          read: ++sequence,
          started: false,
          headers: false,
          insideAbortCall: false,
          aborted: false,
        }
        metadata.set(this, meta)
        const recordRead = (kind: AvailabilityReadEvent['kind'], detail: Partial<AvailabilityReadEvent> = {}) => {
          record({
            kind,
            document: documentId,
            read: meta.read,
            aborted: meta.aborted,
            afterPagehide: pageHidden,
            ...detail,
          })
        }

        const settled = (kind: AvailabilityReadEvent['kind'], detail: Partial<AvailabilityReadEvent> = {}) => queueMicrotask(() => recordRead(kind, detail))
        const nativeEvent = (event: ProgressEvent, type: string) => event.type === type && event.isTrusted
          && event instanceof ProgressEvent && event.target === this && event.currentTarget === this
        this.addEventListener('readystatechange', () => {
          if (this.readyState >= 2 && this.status && !meta.headers) {
            meta.headers = true
            recordRead('headers', { status: this.status })
          }
        }, { capture: true })
        this.addEventListener('abort', (event) => {
          const native = nativeEvent(event, 'abort')
          meta.aborted = native && meta.insideAbortCall
          recordRead('abort', { trusted: event.isTrusted, exactTarget: native, insideAbortCall: meta.insideAbortCall })
          if (native)
            settled('rejected')
        })
        for (const kind of ['error', 'timeout'] as const) {
          this.addEventListener(kind, (event) => {
            if (nativeEvent(event, kind))
              settled('rejected')
          })
        }

        this.addEventListener('load', (event) => {
          if (!nativeEvent(event, 'load') || this.status !== 200)
            return
          try {
            const value = JSON.parse(this.responseText) as unknown
            const exactBody = window.cairnExpectedAvailability !== undefined
              && JSON.stringify(value) === JSON.stringify(window.cairnExpectedAvailability)
            settled('body', { status: this.status, exactBody })
          }
          catch {
            settled('body-rejected')
          }
        })
      }

      return open.call(this, method, url, async, username, password)
    }

    XMLHttpRequest.prototype.send = function (this: XMLHttpRequest, body?: Document | XMLHttpRequestBodyInit | null) {
      const meta = metadata.get(this)
      if (meta && !meta.started) {
        meta.started = true
        record({
          kind: 'start',
          document: documentId,
          read: meta.read,
          aborted: false,
          afterPagehide: pageHidden,
        })
      }

      return send.call(this, body)
    }

    XMLHttpRequest.prototype.abort = function (this: XMLHttpRequest) {
      const meta = metadata.get(this)
      if (!meta)
        return abort.call(this)
      meta.insideAbortCall = true
      record({
        kind: 'abort-call-enter',
        document: documentId,
        read: meta.read,
        aborted: false,
        insideAbortCall: true,
        afterPagehide: pageHidden,
      })
      try {
        return abort.call(this)
      }
      finally {
        meta.insideAbortCall = false
        record({
          kind: 'abort-call-return',
          document: documentId,
          read: meta.read,
          aborted: meta.aborted,
          insideAbortCall: false,
          afterPagehide: pageHidden,
        })
      }
    }
  })
}

// A final journal must account for every producer, including retired documents.
// A known HTTP status alone does not establish native request/body settlement.
export function incompleteAvailabilityReads(reads: readonly AvailabilityReadEvent[]) {
  const starts = reads.filter(event => event.kind === 'start')
  const terminals = reads.filter(event => ['body', 'body-rejected', 'rejected'].includes(event.kind))
  return starts.filter((start) => {
    const sameRead = (event: AvailabilityReadEvent) => event.document === start.document && event.read === start.read
    return starts.filter(sameRead).length !== 1 || terminals.filter(sameRead).length !== 1
  })
}
