import type { Page } from '@playwright/test'
import type { AvailabilityReadEvent } from '../../../tests/e2e/availability-probe'
import assert from 'node:assert/strict'
import {
  afterEach,
  beforeEach,
  it,
  vi,
} from 'vitest'
import { incompleteAvailabilityReads, installAvailabilityProbe } from '../../../tests/e2e/availability-probe'
import { AvailabilityDeparture, readAvailability } from '../src/availability-read'

class NativeProgress {
  isTrusted = true
  constructor(readonly type: string, readonly target: unknown, readonly currentTarget = target) {}
}

class NativeBeforeUnload {
  type = 'beforeunload'
  isTrusted = true
  constructor(readonly target: unknown, readonly currentTarget: unknown) {}
}

const listeners = new Set<(event: NativeBeforeUnload) => void>()
let ownerWindow: Record<string, unknown>
let ownerDocument: Record<string, unknown>

class Request {
  static instances: Request[] = []
  readyState = 0
  status = 0
  responseText = ''
  withCredentials = false
  abortMode: 'sync' | 'deferred' | 'error' | 'none' = 'sync'
  onreadystatechange: (() => void) | null = null
  onabort: ((event: NativeProgress) => void) | null = null
  onload: ((event: NativeProgress) => void) | null = null
  onerror: ((event: NativeProgress) => void) | null = null
  ontimeout: ((event: NativeProgress) => void) | null = null
  method = ''
  url = ''
  abortListenerCounts: number[] = []
  events = new Map<string, Array<{ callback: (event: NativeProgress) => void, capture: boolean }>>()
  constructor() { Request.instances.push(this) }
  open(method: string, url: string) {
    this.method = method
    this.url = url
    this.readyState = 1
    this.onreadystatechange?.()
  }

  send() {}
  addEventListener(type: string, callback: (event: NativeProgress) => void, options?: { capture?: boolean }) {
    this.events.set(type, [...(this.events.get(type) || []), { callback, capture: options?.capture === true }])
  }

  nativeAbort() {
    const event = new NativeProgress('abort', this)
    this.status = 0
    this.onabort?.(event)
    for (const listener of this.events.get('abort') || [])
      listener.callback(event)
  }

  abort() {
    this.abortListenerCounts.push(listeners.size)
    if (this.abortMode === 'sync')
      this.nativeAbort()
    else if (this.abortMode === 'error')
      this.onerror?.(new NativeProgress('error', this))
    else if (this.abortMode === 'deferred')
      queueMicrotask(() => this.onabort?.(new NativeProgress('abort', this)))
  }

  headers(status: number) {
    this.status = status
    this.readyState = 2
    this.onreadystatechange?.()
  }

  async observedHeaders(status: number, checkpoint: () => Promise<unknown>) {
    this.status = status
    this.readyState = 2
    const event = new NativeProgress('readystatechange', this)
    for (const listener of this.events.get('readystatechange') || []) {
      if (listener.capture)
        listener.callback(event)
    }
    this.onreadystatechange?.()
    // DOM callbacks drain application microtasks before invoking the next
    // ordinary listener. The caller can dispose and reset the XHR's status.
    await checkpoint()
    for (const listener of this.events.get('readystatechange') || []) {
      if (!listener.capture)
        listener.callback(event)
    }
  }

  load(text: string, status = 200) {
    this.headers(status)
    this.responseText = text
    this.readyState = 4
    this.onload?.(new NativeProgress('load', this))
  }
}

function departure(event = new NativeBeforeUnload(ownerDocument, ownerWindow)) {
  for (const listener of [...listeners])
    listener(event)
}

beforeEach(() => {
  Request.instances = []
  listeners.clear()
  ownerWindow = {
    BeforeUnloadEvent: NativeBeforeUnload,
    ProgressEvent: NativeProgress,
    XMLHttpRequest: Request,
    addEventListener(type: string, listener: (event: NativeBeforeUnload) => void) {
      if (type === 'beforeunload')
        listeners.add(listener)
    },
    removeEventListener(type: string, listener: (event: NativeBeforeUnload) => void) {
      if (type === 'beforeunload')
        listeners.delete(listener)
    },
  }
  ownerDocument = { defaultView: ownerWindow }
  ownerWindow.document = ownerDocument
  ownerWindow.top = ownerWindow
  vi.stubGlobal('window', ownerWindow)
  vi.stubGlobal('document', ownerDocument)
  vi.stubGlobal('XMLHttpRequest', Request)
})
afterEach(() => vi.unstubAllGlobals())

it('uses one same-origin GET, delivers headers promptly and consumes the exact native body once', async () => {
  const read = readAvailability(new AbortController().signal)
  const request = Request.instances[0]!
  assert.equal(request.method, 'GET')
  assert.equal(request.url, '/api/installations')
  assert.equal(request.withCredentials, false)
  request.headers(200)
  const response = await read.response
  assert.equal(response.ok, true)
  const parsed = response.json()
  assert.equal(listeners.size, 1)
  request.load('[{"online":true,"updateRequired":false}]')
  assert.deepEqual(await parsed, [{ online: true, updateRequired: false }])
  await assert.rejects(response.json(), TypeError)
  assert.equal(listeners.size, 0)
  read.dispose()
  assert.equal(request.abortListenerCounts.length, 0)
})

it('withdraws only in the trusted native abort handler inside its own call, before a second departure', async () => {
  for (let attempt = 0; attempt < 2; attempt++) {
    const read = readAvailability(new AbortController().signal)
    const rejection = assert.rejects(read.response, AvailabilityDeparture)
    departure()
    await rejection
    assert.deepEqual(Request.instances.at(-1)!.abortListenerCounts, [0])
    assert.equal(listeners.size, 0)
  }
})

it('keeps the listener after untrusted, foreign-target or wrong-interface events', async () => {
  const read = readAvailability(new AbortController().signal)
  const rejection = assert.rejects(read.response, AvailabilityDeparture)
  const untrusted = new NativeBeforeUnload(ownerDocument, ownerWindow)
  untrusted.isTrusted = false
  departure(untrusted)
  departure(new NativeBeforeUnload({}, ownerWindow))
  departure(new NativeBeforeUnload(ownerDocument, {}))
  departure({
    type: 'beforeunload',
    isTrusted: true,
    target: ownerDocument,
    currentTarget: ownerWindow,
  })
  assert.equal(listeners.size, 1)
  assert.equal(Request.instances[0]!.abortListenerCounts.length, 0)
  departure()
  await rejection
})

it('rejects a late native abort as a genuine failure rather than a departure', async () => {
  const read = readAvailability(new AbortController().signal)
  Request.instances[0]!.abortMode = 'deferred'
  const rejection = assert.rejects(read.response, cause => cause instanceof DOMException && !(cause instanceof AvailabilityDeparture))
  departure()
  await rejection
})

it('never converts native error during abort or a previously settled error into departure', async () => {
  const read = readAvailability(new AbortController().signal)
  Request.instances[0]!.abortMode = 'error'
  const rejection = assert.rejects(read.response, TypeError)
  departure()
  await rejection
  const prior = readAvailability(new AbortController().signal)
  const priorRequest = Request.instances.at(-1)!
  const priorFailure = assert.rejects(prior.response, TypeError)
  priorRequest.onerror?.(new NativeProgress('error', priorRequest))
  departure()
  prior.dispose()
  await priorFailure
  assert.equal(priorRequest.abortListenerCounts.length, 0)
  assert.equal(listeners.size, 0)
})

it('does not treat an armed state, pending request or synthetic callback as a native terminal', async () => {
  const controller = new AbortController()
  const read = readAvailability(controller.signal)
  const request = Request.instances[0]!
  request.abortMode = 'none'
  let settled = false
  void read.response.then(() => settled = true, () => settled = true)
  departure()
  const synthetic = new NativeProgress('abort', request)
  synthetic.isTrusted = false
  request.onabort?.(synthetic)
  await Promise.resolve()
  assert.equal(settled, false)
  controller.abort()
  await assert.rejects(read.response, DOMException)
  assert.equal(listeners.size, 0)
})

it('observes 401 and HTTP failure headers before a stalled body and disposes the unread body', async () => {
  for (const status of [401, 503]) {
    const read = readAvailability(new AbortController().signal)
    Request.instances.at(-1)!.headers(status)
    const response = await read.response
    assert.equal(response.status, status)
    assert.equal(response.ok, false)
    read.dispose()
    assert.equal(listeners.size, 0)
    await assert.rejects(response.json(), DOMException)
  }
})

it('preserves native body failure, malformed JSON and status zero as failures', async () => {
  const read = readAvailability(new AbortController().signal)
  const request = Request.instances[0]!
  request.headers(200)
  const response = await read.response
  const rejected = assert.rejects(response.json(), TypeError)
  request.onerror?.(new NativeProgress('error', request))
  departure()
  await rejected
  const malformed = readAvailability(new AbortController().signal)
  Request.instances.at(-1)!.load('not JSON')
  await assert.rejects((await malformed.response).json(), SyntaxError)
  const zero = readAvailability(new AbortController().signal)
  const failure = assert.rejects(zero.response, TypeError)
  Request.instances.at(-1)!.load('', 0)
  await failure
})

it('aborts body consumption on explicit caller withdrawal and detaches all listeners', async () => {
  const controller = new AbortController()
  const read = readAvailability(controller.signal)
  Request.instances[0]!.headers(200)
  const body = (await read.response).json()
  const rejection = assert.rejects(body, DOMException)
  controller.abort()
  await rejection
  assert.deepEqual(Request.instances[0]!.abortListenerCounts, [0])
  assert.equal(listeners.size, 0)
  const before = Request.instances[0]!.abortListenerCounts.length
  departure()
  read.dispose()
  assert.equal(Request.instances[0]!.abortListenerCounts.length, before)
})

it('does not start an already withdrawn caller', async () => {
  const controller = new AbortController()
  controller.abort()
  await assert.rejects(readAvailability(controller.signal).response, DOMException)
  assert.equal(Request.instances[0]!.method, '')
  assert.equal(listeners.size, 0)
})

it('observes genuine HTTP headers before application disposal resets the native status', async () => {
  const storage = new Map<string, string>()
  vi.stubGlobal('ProgressEvent', NativeProgress)
  vi.stubGlobal('sessionStorage', {
    getItem: (key: string) => storage.get(key) ?? null,
    setItem: (key: string, value: string) => storage.set(key, value),
  })
  const page = {
    async exposeFunction(name: string, callback: (event: unknown) => void) {
      ownerWindow[name] = async (event: unknown) => callback(event)
    },
    async addInitScript(callback: () => void) { callback() },
  }
  await installAvailabilityProbe(page as unknown as Page, () => {})
  const read = readAvailability(new AbortController().signal)
  let rejectedHttp = false
  const application = read.response.then((response) => {
    assert.equal(response.status, 503)
    rejectedHttp = !response.ok
  }).finally(() => read.dispose())
  await Request.instances[0]!.observedHeaders(503, () => application)
  assert.equal(rejectedHttp, true)
  assert.equal(Request.instances[0]!.status, 0)
  const journal = JSON.parse(storage.get('cairn-e2e-availability-journal') || '[]') as Array<{ kind: string, status?: number }>
  assert.equal(journal.filter(event => event.kind === 'headers' && event.status === 503).length, 1)
})

it('requires exactly one terminal for each current or retired journal read', () => {
  const start: AvailabilityReadEvent = {
    sequence: 1,
    kind: 'start',
    document: 'retired-document',
    read: 1,
    aborted: false,
    afterPagehide: false,
  }
  const headers: AvailabilityReadEvent = { ...start, sequence: 2, kind: 'headers', status: 503 }
  const terminal: AvailabilityReadEvent = { ...start, sequence: 3, kind: 'rejected', aborted: true }
  assert.deepEqual(incompleteAvailabilityReads([start]), [start])
  assert.deepEqual(incompleteAvailabilityReads([start, headers]), [start])
  assert.deepEqual(incompleteAvailabilityReads([start, { ...terminal, document: 'current-document' }]), [start])
  assert.deepEqual(incompleteAvailabilityReads([start, { ...terminal, read: 2 }]), [start])
  assert.deepEqual(incompleteAvailabilityReads([start, headers, terminal]), [])
  assert.deepEqual(incompleteAvailabilityReads([start, { ...terminal, kind: 'body', exactBody: true }]), [])
  assert.deepEqual(incompleteAvailabilityReads([start, { ...terminal, kind: 'body-rejected' }]), [])
  assert.deepEqual(incompleteAvailabilityReads([start, terminal, { ...terminal, sequence: 4 }]), [start])
  assert.deepEqual(incompleteAvailabilityReads([start, { ...start, sequence: 2 }, terminal]), [start, { ...start, sequence: 2 }])
})
