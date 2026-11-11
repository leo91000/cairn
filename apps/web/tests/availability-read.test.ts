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
const hiddenListeners = new Set<(event: Intent) => void>()
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

class PageHide extends Intent {
  override type = 'pagehide'
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

function hide(event = new PageHide(ownerDocument, ownerWindow)) {
  for (const listener of [...hiddenListeners])
    listener(event)
}

beforeEach(() => {
  calls = []
  listeners.clear()
  hiddenListeners.clear()
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
    PageTransitionEvent: PageHide,
    addEventListener(type: string, callback: (event: Intent) => void, capture?: boolean) {
      if (type === 'beforeunload')
        listeners.add(callback)
      if (type === 'pagehide' && capture === true)
        hiddenListeners.add(callback)
    },
    removeEventListener(type: string, callback: (event: Intent) => void, capture?: boolean) {
      if (type === 'beforeunload')
        listeners.delete(callback)
      if (type === 'pagehide' && capture === true)
        hiddenListeners.delete(callback)
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

it('captures a later read at native pagehide while requiring its original body terminal', async () => {
  loseReason = true
  const first = readAvailability(new HostController().signal)
  const initial = assert.rejects(first.response, AvailabilityDeparture)
  departure()
  await initial
  const next = readAvailability(new HostController().signal)
  assert.equal(hiddenListeners.size, 1)
  headers()
  const body = (await next.response).json()
  const rejected = assert.rejects(body, AvailabilityDeparture)
  const synthetic = new PageHide(ownerDocument, ownerWindow)
  synthetic.isTrusted = false
  hide(synthetic)
  hide(new PageHide({}, ownerWindow))
  hide(new PageHide(ownerDocument, {}))
  hide({
    type: 'pagehide',
    isTrusted: true,
    target: ownerDocument,
    currentTarget: ownerWindow,
  } as PageHide)
  assert.equal(calls.at(-1)!.aborts, 0)
  assert.equal(hiddenListeners.size, 1)
  hide()
  await rejected
  assert.equal(hiddenListeners.size, 0)
  assert.equal(listeners.size, 0)
})

it('preserves a body failure settled before captured pagehide', async () => {
  const read = readAvailability(new HostController().signal)
  assert.equal(hiddenListeners.size, 1)
  headers()
  const body = (await read.response).json()
  const cause = new TypeError('genuine earlier native body failure')
  const rejected = assert.rejects(body, error => error === cause)
  failure(cause)
  hide()
  await rejected
  assert.equal(hiddenListeners.size, 0)
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

async function installSourceProbe() {
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
  return storage
}

it('observes the original fetch and JSON promises without clones, replacement promises or missing HTTP status', async () => {
  const storage = await installSourceProbe()
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

  const partials: AvailabilityReadEvent[][] = [
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
    [...complete, {
      ...abort,
      sequence: 6,
      trusted: false,
      exactTarget: false,
      insideAbortCall: false,
    }],
    [...complete, { ...enter, sequence: 6 }, { ...returned, sequence: 7 }],
    [...complete, { ...enter, sequence: 6 }, { ...abort, sequence: 7, trusted: false }, { ...returned, sequence: 8 }],
    [...complete, { ...enter, sequence: 6 }, { ...abort, sequence: 7, insideAbortCall: false }, { ...returned, sequence: 8 }],
    [...complete, { ...enter, sequence: 6 }, { ...returned, sequence: 7 }, { ...abort, sequence: 8 }],
    [start, {
      ...terminal,
      kind: 'rejected',
      sequence: 2,
      aborted: true,
    }, { ...enter, sequence: 3 }, { ...abort, sequence: 4 }, { ...returned, sequence: 5 }],
    [start, response, headers, body, {
      ...terminal,
      kind: 'body-rejected',
      sequence: 5,
      aborted: true,
    }, { ...enter, sequence: 6 }, { ...abort, sequence: 7 }, { ...returned, sequence: 8 }],
  ]
  for (const partial of partials) {
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
  assert.deepEqual(incompleteAvailabilityReads([
    start,
    { ...enter, sequence: 2 },
    { ...returned, sequence: 3 },
    { ...abort, sequence: 4, insideAbortCall: false },
    {
      ...terminal,
      kind: 'rejected',
      sequence: 5,
      aborted: false,
      errorName: 'AbortError',
    },
  ]), [])
  assert.deepEqual(incompleteAvailabilityReads([{ ...start, kind: 'pagehide', read: 0 }, { ...start, kind: 'pageshow', read: 0 }]), [])
})

it('forwards the exact fetch arguments and receiver, including an empty argument list', async () => {
  const original = Promise.resolve(new NativeResponse({} as Call, 200))
  const received: { receiver: unknown, args: unknown[] }[] = []
  ownerWindow.fetch = function (...args: unknown[]) {
    received.push({ receiver: this, args })
    return original
  }

  await installSourceProbe()
  const fetch = ownerWindow.fetch as typeof globalThis.fetch
  const receiver = {}
  for (const args of [[], ['/untracked'], ['/untracked', undefined, 'extra']]) {
    assert.equal(Reflect.apply(fetch, receiver, args), original)
    assert.equal(received.at(-1)!.receiver, receiver)
    assert.deepEqual(received.at(-1)!.args, args)
  }

  await original
})

it('leaves fetch input coercion and the identical original rejection to the original function', async () => {
  const cause = new TypeError('original fetch input refusal')
  const original = Promise.reject(cause)
  void original.catch(() => {})
  const input = {
    toString() {
      throw cause
    },
  }
  let invoked = 0
  ownerWindow.fetch = function (...args: unknown[]) {
    invoked++
    assert.equal(args[0], input)
    return original
  }

  const storage = await installSourceProbe()
  const fetch = ownerWindow.fetch as typeof globalThis.fetch
  let returned: Promise<Response> | undefined
  assert.doesNotThrow(() => {
    returned = Reflect.apply(fetch, ownerWindow, [input])
  })
  assert.equal(returned, original)
  await assert.rejects(returned!, value => value === cause)
  assert.equal(invoked, 1)
  const journal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
  assert.equal(journal.some(event => event.kind === 'observer-failed'), true)
  assert.equal(incompleteAvailabilityReads(journal).length > 0, true)
})

it('never consumes method or signal accessors a second time for metadata', async () => {
  let methodReads = 0
  let signalReads = 0
  const signal = new HostController().signal
  const init = {
    get method() {
      methodReads++
      return methodReads === 1 ? 'GET' : 'POST'
    },
    get signal() {
      signalReads++
      return signal
    },
  }
  const original = Promise.resolve(new NativeResponse({} as Call, 200))
  ownerWindow.fetch = (_input: unknown, options: RequestInit) => {
    assert.equal(options.method, 'GET')
    assert.equal(options.signal, signal)
    return original
  }

  const storage = await installSourceProbe()
  const returned = Reflect.apply(ownerWindow.fetch as typeof fetch, ownerWindow, ['/api/installations', init])
  assert.equal(returned, original)
  await returned
  assert.equal(methodReads, 1)
  assert.equal(signalReads, 1)
  const journal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
  assert.equal(journal.some(event => event.kind === 'observer-failed'), true)
  assert.equal(incompleteAvailabilityReads(journal).length > 0, true)
})

it('preserves an original synchronous fetch exception without a metadata exception', async () => {
  const cause = new TypeError('original synchronous native refusal')
  let invoked = 0
  ownerWindow.fetch = () => {
    invoked++
    throw cause
  }

  await installSourceProbe()
  const input = {
    toString() {
      throw new Error('metadata must not run first')
    },
  }
  assert.throws(() => Reflect.apply(ownerWindow.fetch as typeof fetch, ownerWindow, [input]), error => error === cause)
  assert.equal(invoked, 1)
})

it('keeps original active fetch and JSON failures while recording distinct read phases', async () => {
  const storage = await installSourceProbe()
  const fetch = ownerWindow.fetch as typeof globalThis.fetch
  const first = fetch('/api/installations', { signal: new HostController().signal })
  const fetchCause = new TypeError('same original active fetch rejection')
  const rejected = assert.rejects(first, error => error === fetchCause)
  failure(fetchCause)
  await rejected
  const second = fetch('/api/installations', { signal: new HostController().signal })
  headers()
  const body = (await second).json()
  const bodyCause = new SyntaxError('same original native JSON rejection')
  const bodyRejected = assert.rejects(body, error => error === bodyCause)
  failure(bodyCause)
  await bodyRejected
  await flush()
  const journal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
  assert.deepEqual(incompleteAvailabilityReads(journal), [])
  assert.equal(journal.filter(event => event.kind === 'rejected' && event.errorName === 'TypeError' && !event.aborted).length, 1)
  assert.equal(journal.filter(event => event.kind === 'body-rejected' && event.errorName === 'SyntaxError' && !event.aborted).length, 1)
  assert.equal(ownerWindow.cairnAvailabilityProbeFailures, 0)
})

it('forwards JSON arguments and actual receiver while keeping the original promise', async () => {
  const response = new NativeResponse({} as Call, 200)
  const originalBody = Promise.resolve({ original: true })
  const received: { receiver: unknown, args: unknown[] }[] = []
  NativeResponse.prototype.json = function (...input: unknown[]) {
    received.push({ receiver: this, args: input })
    return originalBody
  }

  ownerWindow.fetch = () => Promise.resolve(response)
  await installSourceProbe()
  const returned = await Reflect.apply(ownerWindow.fetch as typeof fetch, ownerWindow, ['/api/installations'])
  const original = Reflect.apply(returned.json, returned, ['extra', undefined])
  assert.equal(original, originalBody)
  assert.deepEqual(await original, { original: true })
  assert.equal(received[0]!.receiver, response)
  assert.deepEqual(received[0]!.args, ['extra', undefined])
})

it('forwards abort arguments and actual signal ownership without reading an instance accessor', async () => {
  const controller = new NativeController()
  const signal = controller.signal
  Object.defineProperty(controller, 'signal', {
    get() {
      throw new Error('instance accessor must not be inspected')
    },
  })
  const original = Promise.resolve(new NativeResponse({} as Call, 200))
  const received: { receiver: unknown, args: unknown[] }[] = []
  NativeController.prototype.abort = function (...args: unknown[]) {
    received.push({ receiver: this, args })
    Reflect.apply(hostAbort, this, args)
  }

  ownerWindow.fetch = () => original
  await installSourceProbe()
  assert.equal(Reflect.apply(ownerWindow.fetch as typeof fetch, ownerWindow, ['/api/installations', { signal }]), original)
  const reason = new DOMException('owned reason', 'AbortError')
  assert.doesNotThrow(() => Reflect.apply(controller.abort, controller, [reason, 'extra']))
  assert.equal(received[0]!.receiver, controller)
  assert.deepEqual(received[0]!.args, [reason, 'extra'])
  assert.equal(signal.reason, reason)
  const unrelated = new NativeController()
  unrelated.abort()
  assert.deepEqual(received[1]!.args, [])
})

it('keeps native promises unchanged when observation storage or delivery fails, with an explicit failure receipt', async () => {
  const original = Promise.resolve(new NativeResponse({} as Call, 200))
  ownerWindow.fetch = () => original
  await installSourceProbe()
  const failures: AvailabilityReadEvent[] = []
  vi.stubGlobal('sessionStorage', {
    getItem() {
      throw new Error('original unavailable observer storage')
    },
  })
  ownerWindow.cairnAvailabilityRecord = async (event: AvailabilityReadEvent) => {
    failures.push(event)
  }

  assert.equal(Reflect.apply(ownerWindow.fetch as typeof fetch, ownerWindow, ['/api/installations']), original)
  await original
  await flush()
  assert.equal((ownerWindow.cairnAvailabilityProbeFailures as number) > 0, true)
  assert.equal(failures.some(event => event.kind === 'observer-failed'), true)
})

it.each([false, true])('rejects cross-option signal mutation even when the accessor removes itself (%s)', async (removesAccessor) => {
  const actual = new NativeController()
  const unrelated = new NativeController()
  const native = Promise.withResolvers<NativeResponse>()
  const cause = new HostException('original actual-request failure', 'AbortError')
  const options: RequestInit = { signal: actual.signal }
  Object.defineProperty(options, 'window', {
    configurable: true,
    enumerable: true,
    get() {
      options.signal = unrelated.signal
      if (removesAccessor)
        Reflect.deleteProperty(options, 'window')
      return undefined
    },
  })
  let used: AbortSignal | null | undefined
  ownerWindow.fetch = (_input: unknown, init: RequestInit) => {
    used = init.signal
    void init.window
    used!.addEventListener('abort', () => native.reject(cause), { once: true })
    return native.promise
  }

  const storage = await installSourceProbe()
  const returned = Reflect.apply(ownerWindow.fetch as typeof fetch, ownerWindow, ['/api/installations', options])
  assert.equal(returned, native.promise)
  assert.equal(used, actual.signal)
  unrelated.abort(new HostException('different unrelated reason', 'AbortError'))
  const rejected = assert.rejects(returned, error => error === cause)
  actual.abort(cause)
  await rejected
  await flush()
  const journal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
  assert.equal(journal.some(event => event.kind === 'observer-failed'), true)
  assert.equal(journal.some(event => event.kind === 'rejected' && event.aborted), false)
  assert.equal(incompleteAvailabilityReads(journal).length > 0, true)
})
