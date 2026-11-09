import type { Page } from '@playwright/test'
import type { AvailabilityReadEvent } from '../../../tests/e2e/availability-probe'
import assert from 'node:assert/strict'
import {
  afterEach,
  beforeEach,
  it,
  vi,
} from 'vitest'
import { availabilityBodiesAfter, incompleteAvailabilityReads, installAvailabilityProbe } from '../../../tests/e2e/availability-probe'
import { AvailabilityDeparture, readAvailability } from '../src/availability-read'

const HostController = AbortController
const hostAbort = AbortController.prototype.abort
const HostEvent = Event
const HostException = DOMException
const listeners = new Set<(event: Intent) => void>()
let ownerWindow: Record<string, unknown>
let ownerDocument: Record<string, unknown>
let calls: Call[]
let loseReason = false
let deferAbort = false
let omitAbort = false

class Intent {
  type = 'beforeunload'
  isTrusted = true
  constructor(readonly target: unknown, readonly currentTarget: unknown) {}
}

interface Call {
  input: unknown
  init: RequestInit
  response: ReturnType<typeof Promise.withResolvers<NativeResponse>>
  headerStatus?: number
  outcome?: { value: unknown } | { error: unknown }
  body?: ReturnType<typeof Promise.withResolvers<unknown>>
  jsonCalls: number
  aborts: number
}

class NativeController extends HostController {
  abort(reason?: unknown) {
    if (omitAbort)
      return
    if (deferAbort) {
      queueMicrotask(() => hostAbort.call(this, reason))
      return
    }

    hostAbort.call(this, reason)
  }
}

class NativeResponse {
  constructor(readonly call: Call, readonly status: number) {}
  get ok() { return this.status >= 200 && this.status < 300 }
  get body() { throw new Error('The availability factory must not create or expose a stream') }
  clone() { throw new Error('The availability factory must not clone its native response') }
  json() {
    this.call.jsonCalls++
    if (this.call.outcome)
      return 'value' in this.call.outcome ? Promise.resolve(this.call.outcome.value) : Promise.reject(this.call.outcome.error)
    this.call.body ??= Promise.withResolvers<unknown>()
    return this.call.body.promise
  }
}

const originalJson = NativeResponse.prototype.json
const originalAbort = NativeController.prototype.abort

function headers(status = 200, call = calls.at(-1)!) {
  call.headerStatus = status
  call.response.resolve(new NativeResponse(call, status))
}

function value(data: unknown, call = calls.at(-1)!) {
  call.outcome = { value: data }
  call.body?.resolve(data)
}

function failure(cause: unknown, call = calls.at(-1)!) {
  if (call.headerStatus === undefined) {
    call.response.reject(cause)
  }
  else {
    call.outcome = { error: cause }
    call.body?.reject(cause)
  }
}

async function flush() {
  for (let index = 0; index < 10; index++)
    await Promise.resolve()
}

function departure(event = new Intent(ownerDocument, ownerWindow)) {
  for (const listener of [...listeners])
    listener(event)
}

beforeEach(() => {
  calls = []
  listeners.clear()
  loseReason = false
  deferAbort = false
  omitAbort = false
  NativeResponse.prototype.json = originalJson
  NativeController.prototype.abort = originalAbort
  const fetch = (input: unknown, init: RequestInit) => {
    const call: Call = {
      input,
      init,
      response: Promise.withResolvers<NativeResponse>(),
      jsonCalls: 0,
      aborts: 0,
    }
    calls.push(call)
    init.signal?.addEventListener('abort', () => {
      if (!init.signal?.aborted)
        return
      call.aborts++
      failure(loseReason ? new HostException('Fetch is aborted', 'AbortError') : init.signal.reason, call)
    }, { once: true })
    return call.response.promise
  }

  ownerWindow = {
    fetch,
    Response: NativeResponse,
    AbortController: NativeController,
    Event: HostEvent,
    DOMException: HostException,
    BeforeUnloadEvent: Intent,
    addEventListener(type: string, callback: (event: Intent) => void) {
      if (type === 'beforeunload')
        listeners.add(callback)
    },
    removeEventListener(type: string, callback: (event: Intent) => void) {
      if (type === 'beforeunload')
        listeners.delete(callback)
    },
  }
  ownerDocument = { defaultView: ownerWindow }
  ownerWindow.document = ownerDocument
  ownerWindow.top = ownerWindow
  vi.stubGlobal('window', ownerWindow)
  vi.stubGlobal('document', ownerDocument)
  vi.stubGlobal('Response', NativeResponse)
  vi.stubGlobal('AbortController', NativeController)
})

afterEach(() => {
  NativeResponse.prototype.json = originalJson
  NativeController.prototype.abort = originalAbort
  vi.unstubAllGlobals()
})

it('uses one private same-origin keepalive GET and only the original native JSON consumer', async () => {
  const parent = new HostController()
  const read = readAvailability(parent.signal)
  const call = calls[0]!
  assert.equal(call.input, '/api/installations')
  assert.equal(call.init.credentials, 'same-origin')
  assert.equal(call.init.keepalive, true)
  assert.notEqual(call.init.signal, parent.signal)
  assert.equal(listeners.size, 1)
  headers()
  const response = await read.response
  assert.equal(response.status, 200)
  assert.equal(response.ok, true)
  const body = response.json()
  value([{ id: 'installation', online: true }])
  assert.deepEqual(await body, [{ id: 'installation', online: true }])
  await assert.rejects(response.json(), TypeError)
  assert.equal(call.jsonCalls, 1)
  assert.equal(listeners.size, 0)
  read.dispose()
  parent.abort()
  assert.equal(call.aborts, 0)
})

it('requires a trusted exact native signal event inside its own call and the original fetch terminal', async () => {
  for (const generic of [false, true]) {
    loseReason = generic
    const read = readAvailability(new HostController().signal)
    const rejected = assert.rejects(read.response, AvailabilityDeparture)
    departure()
    await rejected
    assert.equal(calls.at(-1)!.aborts, 1)
    assert.equal(listeners.size, 0)
    read.dispose()
  }
})

it('requires the original body terminal after an owned native signal and rearms the next read', async () => {
  loseReason = true
  const read = readAvailability(new HostController().signal)
  headers()
  const body = (await read.response).json()
  const rejected = assert.rejects(body, AvailabilityDeparture)
  departure()
  await rejected
  assert.equal(listeners.size, 0)
  const next = readAvailability(new HostController().signal)
  headers()
  const nextBody = (await next.response).json()
  value([{ online: true }])
  assert.deepEqual(await nextBody, [{ online: true }])
  assert.equal(listeners.size, 0)
})

it('does not disarm the listener for synthetic, foreign or wrong-interface intent', async () => {
  const read = readAvailability(new HostController().signal)
  const untrusted = new Intent(ownerDocument, ownerWindow)
  untrusted.isTrusted = false
  departure(untrusted)
  departure(new Intent({}, ownerWindow))
  departure(new Intent(ownerDocument, {}))
  departure({
    type: 'beforeunload',
    isTrusted: true,
    target: ownerDocument,
    currentTarget: ownerWindow,
  } as Intent)
  assert.equal(listeners.size, 1)
  assert.equal(calls[0]!.aborts, 0)
  const rejected = assert.rejects(read.response, AvailabilityDeparture)
  departure()
  await rejected
})

it('refuses a genuine abort delivered outside the owned-call bracket even with its unique reason', async () => {
  for (const generic of [false, true]) {
    loseReason = generic
    deferAbort = true
    const read = readAvailability(new HostController().signal)
    const rejected = assert.rejects(read.response, cause => !(cause instanceof AvailabilityDeparture))
    departure()
    await rejected
    assert.equal(calls.at(-1)!.aborts, 1)
    assert.equal(listeners.size, 0)
  }
})

it('keeps a prior fetch or body failure even when intent arrives before its promise reaction', async () => {
  for (const bodyStage of [false, true]) {
    const read = readAvailability(new HostController().signal)
    let original = read.response as Promise<unknown>
    if (bodyStage) {
      headers()
      original = (await read.response).json()
    }

    const cause = new TypeError('genuine native network failure')
    const rejected = assert.rejects(original, error => error === cause)
    failure(cause)
    departure()
    await rejected
    assert.equal(listeners.size, 0)
  }
})

it('preserves already fulfilled HTTP failure headers and never hides their body error', async () => {
  loseReason = true
  for (const status of [401, 503]) {
    const read = readAvailability(new HostController().signal)
    headers(status)
    departure()
    const response = await read.response
    assert.equal(response.status, status)
    assert.equal(response.ok, false)
    await assert.rejects(response.json(), cause => cause instanceof DOMException && !(cause instanceof AvailabilityDeparture))
    assert.equal(listeners.size, 0)
  }
})

it('starts successful native JSON before exposing headers even when the parent has already retired', async () => {
  const parent = new HostController()
  const read = readAvailability(parent.signal)
  headers()
  parent.abort()
  const response = await read.response
  assert.equal(response.status, 200)
  assert.equal(calls[0]!.jsonCalls, 1)
  await assert.rejects(response.json(), cause => cause instanceof DOMException && !(cause instanceof AvailabilityDeparture))
  assert.equal(listeners.size, 0)
})

it('does not manufacture a terminal when an abort produces no original rejection', async () => {
  omitAbort = true
  const read = readAvailability(new HostController().signal)
  let settled = false
  void read.response.then(() => settled = true, () => settled = true)
  departure()
  await flush()
  assert.equal(settled, false)
  assert.equal(calls[0]!.aborts, 0)
  headers()
  const body = (await read.response).json()
  value({ original: 'precise body' })
  assert.deepEqual(await body, { original: 'precise body' })
})

it('does not turn a non-native signal interface into an owned departure', async () => {
  ownerWindow.Event = class DifferentEvent {}
  loseReason = true
  const read = readAvailability(new HostController().signal)
  const rejected = assert.rejects(read.response, cause => !(cause instanceof AvailabilityDeparture))
  departure()
  await rejected
})

it('keeps caller withdrawal distinct and releases lifecycle listeners', async () => {
  loseReason = true
  const parent = new HostController()
  const read = readAvailability(parent.signal)
  const rejected = assert.rejects(read.response, cause => cause instanceof DOMException && !(cause instanceof AvailabilityDeparture))
  parent.abort()
  await rejected
  assert.equal(listeners.size, 0)
  const before = calls[0]!.aborts
  departure()
  read.dispose()
  assert.equal(calls[0]!.aborts, before)
})

it('does not start an already withdrawn caller', async () => {
  const parent = new HostController()
  parent.abort()
  await assert.rejects(readAvailability(parent.signal).response, DOMException)
  assert.equal(calls.length, 0)
  assert.equal(listeners.size, 0)
})

it('preserves parsing failures and a successful original body settled before later intent', async () => {
  const read = readAvailability(new HostController().signal)
  headers()
  const body = (await read.response).json()
  const cause = new SyntaxError('genuine malformed JSON')
  const rejected = assert.rejects(body, error => error === cause)
  failure(cause)
  departure()
  await rejected
  const next = readAvailability(new HostController().signal)
  headers()
  const complete = (await next.response).json()
  value({ original: true })
  departure()
  assert.deepEqual(await complete, { original: true })
})

it('observes the original fetch and JSON promises without clones, replacement promises or missing HTTP status', async () => {
  const storage = new Map<string, string>()
  vi.stubGlobal('sessionStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, data: string) => storage.set(key, data),
  })
  const page = {
    async exposeFunction(name: string, callback: (event: unknown) => void) { ownerWindow[name] = async (event: unknown) => callback(event) },
    async addInitScript(callback: () => void) { callback() },
  }
  await installAvailabilityProbe(page as unknown as Page, () => {})
  const fetch = ownerWindow.fetch as (input: string, init: RequestInit) => Promise<NativeResponse>
  const original = fetch('/api/installations', { signal: new HostController().signal })
  assert.equal(original, calls[0]!.response.promise)
  headers()
  const response = await original
  const body = response.json()
  assert.equal(body, calls[0]!.body!.promise)
  value([{ online: true }])
  await body
  const read = readAvailability(new HostController().signal)
  headers(503)
  const bad = await read.response
  assert.equal(bad.status, 503)
  const beforeDisposal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
  assert.deepEqual(incompleteAvailabilityReads(beforeDisposal.filter(event => event.read === 1)), [])
  assert.equal(incompleteAvailabilityReads(beforeDisposal).length, 1)
  read.dispose()
  await flush()
  const journal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
  assert.equal(journal.filter(event => event.kind === 'headers' && event.status === 503).length, 1)
  assert.equal(journal.filter(event => event.kind === 'body').length, 1)
  assert.equal(calls[1]!.jsonCalls, 0)
  // Node's later AbortSignal listeners can receive currentTarget=null. Positive
  // native disposal provenance is checked in the pinned browser controls.
  assert.equal(journal.filter(event => event.kind === 'abort-call-enter').length, 1)
  assert.equal(journal.filter(event => event.kind === 'abort-call-return').length, 1)
})

it('requires a new read and exact body after the restoration or retained-document boundary', () => {
  const start: AvailabilityReadEvent = {
    sequence: 1,
    kind: 'start',
    document: 'retained',
    read: 2,
    aborted: false,
    afterPagehide: false,
  }
  const oldBody: AvailabilityReadEvent = {
    ...start,
    sequence: 2,
    kind: 'body',
    exactBody: true,
  }
  const restored: AvailabilityReadEvent = {
    ...start,
    sequence: 4,
    read: 0,
    kind: 'pageshow',
    trusted: true,
    persisted: true,
  }
  const previous = [start, oldBody, { ...restored, sequence: 3, kind: 'pagehide' as const }, restored]
  assert.equal(previous.some(event => event.read > 1 && event.kind === 'body' && event.exactBody), true)
  assert.deepEqual(availabilityBodiesAfter(previous, 'retained', restored.sequence), [])
  const freshStart: AvailabilityReadEvent = { ...start, read: 3, sequence: 5 }
  const freshBody: AvailabilityReadEvent = { ...oldBody, read: 3, sequence: 6 }
  assert.deepEqual(availabilityBodiesAfter([...previous, freshStart, freshBody], 'retained', restored.sequence), [freshBody])
  for (const invalid of [
    [...previous, { ...oldBody, sequence: 6 }],
    [...previous, freshBody],
    [...previous, freshStart, { ...freshBody, exactBody: false }],
    [...previous, freshStart, { ...freshBody, afterPagehide: true }],
    [...previous, freshStart, { ...freshBody, document: 'replacement' }],
    [...previous, freshStart, { ...freshStart, sequence: 7 }, freshBody],
  ]) {
    assert.deepEqual(availabilityBodiesAfter(invalid, 'retained', restored.sequence), [])
  }
})

it('accounts for native fetch, optional body and unread HTTP disposal as distinct phases', () => {
  const start: AvailabilityReadEvent = {
    sequence: 1,
    kind: 'start',
    document: 'retired',
    read: 1,
    aborted: false,
    afterPagehide: false,
  }
  const response: AvailabilityReadEvent = {
    ...start,
    sequence: 2,
    kind: 'fetch-fulfilled',
    status: 200,
    ok: true,
  }
  const headers: AvailabilityReadEvent = {
    ...start,
    sequence: 3,
    kind: 'headers',
    status: 200,
  }
  const body: AvailabilityReadEvent = { ...start, sequence: 4, kind: 'body-start' }
  const terminal: AvailabilityReadEvent = {
    ...start,
    sequence: 5,
    kind: 'body',
    exactBody: true,
  }
  const complete = [start, response, headers, body, terminal]
  const enter: AvailabilityReadEvent = {
    ...start,
    sequence: 4,
    kind: 'abort-call-enter',
    insideAbortCall: true,
  }
  const abort: AvailabilityReadEvent = {
    ...start,
    sequence: 5,
    kind: 'abort',
    trusted: true,
    exactTarget: true,
    insideAbortCall: true,
  }
  const returned: AvailabilityReadEvent = {
    ...start,
    sequence: 6,
    kind: 'abort-call-return',
    insideAbortCall: false,
  }
  assert.deepEqual(incompleteAvailabilityReads(complete), [])
  for (const partial of [
    [start],
    [terminal],
    [start, terminal],
    [start, response, headers],
    [...complete, { ...terminal, sequence: 6 }],
    [start, response, { ...response, sequence: 6 }, headers, body, terminal],
    [start, response, headers, { ...headers, sequence: 6 }, body, terminal],
    [start, response, headers, terminal],
    [start, response, headers, { ...body, sequence: 7 }, terminal],
    [start, response, { ...headers, status: 503 }, body, terminal],
    [start, { ...response, status: 503, ok: false }, { ...headers, status: 503 }],
    [start, { ...response, status: 503, ok: false }, { ...headers, status: 503 }, enter, { ...abort, trusted: false }, returned],
  ]) {
    assert.notEqual(incompleteAvailabilityReads(partial).length, 0)
  }

  const unreadFailure = [start, { ...response, status: 503, ok: false }, { ...headers, status: 503 }, enter, abort, returned]
  assert.deepEqual(incompleteAvailabilityReads(unreadFailure), [])
  assert.deepEqual(incompleteAvailabilityReads([start, enter, abort, returned, {
    ...terminal,
    sequence: 7,
    kind: 'rejected',
    aborted: true,
  }]), [])
  assert.deepEqual(incompleteAvailabilityReads([{ ...start, kind: 'pagehide', read: 0 }, { ...start, kind: 'pageshow', read: 0 }]), [])
})
