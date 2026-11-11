import type { Page } from '@playwright/test'

export interface AvailabilityReadEvent {
  sequence: number
  kind: 'start' | 'fetch-fulfilled' | 'headers' | 'body' | 'body-start' | 'rejected' | 'body-rejected' | 'abort' | 'abort-call-enter' | 'abort-call-return' | 'pagehide' | 'pageshow' | 'observer-failed'
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
    cairnAvailabilityProbeFailures: number
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
    const then = Promise.prototype.then
    window.cairnAvailabilityProbeFailures = 0
    const store = (entry: AvailabilityReadEvent) => {
      const journal = JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
      journal.push(entry)
      sessionStorage.setItem('cairn-e2e-availability-journal', JSON.stringify(journal))
    }

    const observationFailed = () => {
      window.cairnAvailabilityProbeFailures++
      const entry: AvailabilityReadEvent = {
        kind: 'observer-failed',
        sequence: ++eventSequence,
        document: documentId,
        read: 0,
        aborted: false,
        afterPagehide: pageHidden,
      }
      // An unavailable sink must not replace an application's native result.
      // Persist/forward the failure independently, without recursive delivery.
      try {
        store(entry)
      }
      catch {}

      try {
        const delivery = window.cairnAvailabilityRecord(entry)
        void Reflect.apply(then, delivery, [undefined, () => {}])
      }
      catch {}
    }

    const record = (event: Omit<AvailabilityReadEvent, 'sequence'>) => {
      const entry = { ...event, sequence: ++eventSequence }
      try {
        store(entry)
      }
      catch {
        observationFailed()
      }

      try {
        const delivery = window.cairnAvailabilityRecord(entry)
        void Reflect.apply(then, delivery, [undefined, observationFailed])
      }
      catch {
        observationFailed()
      }
    }

    const observe = <T>(original: Promise<T>, fulfilled: (value: T) => void, rejected: (cause: unknown) => void) => {
      try {
        const observation = Reflect.apply(then, original, [
          (value: T) => {
            try {
              fulfilled(value)
            }
            catch {
              observationFailed()
            }
          },
          (cause: unknown) => {
            try {
              rejected(cause)
            }
            catch {
              observationFailed()
            }
          },
        ])
        void Reflect.apply(then, observation, [undefined, observationFailed])
      }
      catch {
        observationFailed()
      }
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
    let controllerPrototype: object | null = AbortController.prototype
    let controllerSignal: (() => AbortSignal) | undefined
    while (controllerPrototype && !controllerSignal) {
      controllerSignal = Object.getOwnPropertyDescriptor(controllerPrototype, 'signal')?.get
      controllerPrototype = Object.getPrototypeOf(controllerPrototype)
    }

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

    window.fetch = function (this: Window, ...args: Parameters<typeof fetch>) {
      let signal: AbortSignal | null | undefined
      let tracked = false
      let metadataUnsupported = false
      // Snapshot only ordinary own data before native dictionary conversion.
      // Another option's accessor/coercion must not swap the observed owner.
      try {
        if (typeof args[0] !== 'string') {
          metadataUnsupported = true
        }
        else if (args[0] === '/api/installations') {
          tracked = true
          if (args[1] != null) {
            const prototype = Object.getPrototypeOf(args[1])
            const descriptors = Object.getOwnPropertyDescriptors(args[1])
            const unsupportedValue = Object.entries(descriptors).some(([key, descriptor]) => {
              if (!('value' in descriptor))
                return true
              const value: unknown = descriptor.value
              return key !== 'signal' && value !== null && (typeof value === 'object' || typeof value === 'function')
            })
            const method: unknown = descriptors.method?.value
            if ((prototype !== null && prototype !== Object.prototype) || unsupportedValue
              || (method !== undefined && typeof method !== 'string')) {
              metadataUnsupported = true
            }
            else {
              tracked = method === undefined || method.toUpperCase() === 'GET'
              signal = descriptors.signal?.value
              metadataUnsupported = signal != null && !(signal instanceof AbortSignal)
            }
          }
        }
      }
      catch {
        metadataUnsupported = true
      }

      // Always invoke once with the ORIGINAL arguments/receiver, even if the
      // observation surface is unsupported. Never replace its promise/cause.
      const original = Reflect.apply(fetch, this, args) as ReturnType<typeof fetch>
      if (metadataUnsupported) {
        observationFailed()
        return original
      }

      if (!tracked)
        return original

      const meta: Metadata = {
        read: ++sequence,
        signal,
        insideAbortCall: false,
        nativeSignal: false,
      }
      try {
        if (meta.signal) {
          signals.set(meta.signal, meta)
          meta.signal.addEventListener('abort', (event) => {
            try {
              const native = event.type === 'abort' && event.isTrusted && event instanceof Event
                && event.target === meta.signal && event.currentTarget === meta.signal && meta.signal!.aborted
              meta.nativeSignal = native && meta.insideAbortCall
              recordRead(meta, 'abort', { trusted: event.isTrusted, exactTarget: native, insideAbortCall: meta.insideAbortCall })
            }
            catch {
              observationFailed()
            }
          }, { once: true })
        }

        recordRead(meta, 'start')
        // Observe without replacing the original promise or cloning its Response.
        observe(original, (response) => {
          responses.set(response, meta)
          recordRead(meta, 'fetch-fulfilled', { status: response.status, ok: response.ok })
          recordRead(meta, 'headers', { status: response.status })
        }, cause => rejected(meta, 'rejected', cause))
      }
      catch {
        observationFailed()
      }

      return original
    }

    Response.prototype.json = function (this: Response, ...args: Parameters<typeof json>) {
      const meta = responses.get(this)
      if (!meta)
        return Reflect.apply(json, this, args)
      recordRead(meta, 'body-start')
      let original: ReturnType<typeof json>
      try {
        original = Reflect.apply(json, this, args)
      }
      catch (cause) {
        try {
          rejected(meta, 'body-rejected', cause)
        }
        catch {
          observationFailed()
        }

        throw cause
      }

      observe(original, (value: unknown) => {
        recordRead(meta, 'body', {
          status: this.status,
          exactBody: window.cairnExpectedAvailability !== undefined
            && JSON.stringify(value) === JSON.stringify(window.cairnExpectedAvailability),
        })
      }, cause => rejected(meta, 'body-rejected', cause))
      return original
    }

    AbortController.prototype.abort = function (this: AbortController, ...args: Parameters<typeof abort>) {
      let meta: Metadata | undefined
      try {
        if (controllerSignal)
          meta = signals.get(Reflect.apply(controllerSignal, this, []))
        else
          observationFailed()
      }
      catch {
        observationFailed()
      }

      if (!meta)
        return Reflect.apply(abort, this, args)
      meta.insideAbortCall = true
      recordRead(meta, 'abort-call-enter', { insideAbortCall: true })
      try {
        return Reflect.apply(abort, this, args)
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
