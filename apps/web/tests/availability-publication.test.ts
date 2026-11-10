import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { stripTypeScriptTypes } from 'node:module'
import { clearTimeout as clearNativeTimeout, setTimeout as setNativeTimeout } from 'node:timers'
import { setImmediate } from 'node:timers/promises'
import { runInNewContext } from 'node:vm'
import { MessageChannel as HostMessageChannel } from 'node:worker_threads'
import {
  afterEach,
  beforeEach,
  it,
  vi,
} from 'vitest'
import { AvailabilityDeparture, readAvailability } from '../src/availability-read'

const HostController = AbortController
const HostResponse = Response
const HostEvent = Event
const HostException = DOMException
const pollingSource = readFileSync(new URL('../src/BeaconApp.vue', import.meta.url), 'utf8')
const polling = stripTypeScriptTypes(pollingSource.slice(pollingSource.indexOf('let availabilityTimer:'), pollingSource.indexOf('\nfunction installationStatus(')))
const lifetime = stripTypeScriptTypes(readFileSync(new URL('../src/document-lifetime.ts', import.meta.url), 'utf8'))
  .replace(/^import[^\n]+\n/, '')
  .replace(/^export /gm, '')
const online = [{ id: 'installation', online: true, updateRequired: false }]

class BeforeUnload {
  readonly type: string = 'beforeunload'
  readonly isTrusted = true

  constructor(readonly target: unknown, readonly currentTarget: unknown) {}
}

class PageTransition extends BeforeUnload {
  override readonly type: string

  constructor(target: unknown, currentTarget: unknown, type: string, readonly persisted = true) {
    super(target, currentTarget)
    this.type = type
  }
}

interface Timer {
  callback: () => void
  delay: number
}

interface Request {
  headers: ReturnType<typeof Promise.withResolvers<Response>>
  signal: AbortSignal
  body?: ReadableStreamDefaultController<Uint8Array>
}

interface Snapshot {
  failures: number
  request: AbortController | undefined
  generation: AbortController
}

interface Control {
  refreshAvailability: () => Promise<void>
  snapshot: () => Snapshot
  departDocument: () => void
}

function account() {
  return { authenticated: true, installations: online.map(item => ({ ...item })) }
}

function fixture(nativePublication = false) {
  const timers = new Map<number, Timer>()
  const requests: Request[] = []
  const bodies: Promise<unknown>[] = []
  const watchers: Array<() => void> = []
  const disposals: Array<() => void> = []
  const redirects: string[] = []
  const published = Promise.withResolvers<void>()
  const documentListeners = new Map<string, Set<() => void>>()
  const listeners = new Map<string, Set<(event: BeforeUnload) => void>>()
  let nextTimer = 0
  let offlineWrites = 0
  const session = { value: account() }
  const state = new Proxy({
    installationId: 'installation',
    installationOnline: true,
    installationUpdateRequired: false,
    signingOut: false,
    redirecting: false,
  }, {
    set(target, name, value) {
      if (name === 'installationOnline' && value === false) {
        offlineWrites++
        published.resolve()
      }

      Reflect.set(target, name, value)
      return true
    },
  })
  const document = {
    hidden: false,
    defaultView: undefined as unknown,
    addEventListener(name: string, callback: () => void) {
      if (!documentListeners.has(name))
        documentListeners.set(name, new Set())
      documentListeners.get(name)!.add(callback)
    },
    removeEventListener(name: string, callback: () => void) {
      documentListeners.get(name)?.delete(callback)
    },
  }
  const window = {
    document,
    top: undefined as unknown,
    location: { href: 'http://fixture.invalid/mcps' },
    AbortController: HostController,
    Response: HostResponse,
    Event: HostEvent,
    DOMException: HostException,
    BeforeUnloadEvent: BeforeUnload,
    PageTransitionEvent: PageTransition,
    addEventListener(name: string, callback: (event: BeforeUnload) => void) {
      if (!listeners.has(name))
        listeners.set(name, new Set())
      listeners.get(name)!.add(callback)
    },
    removeEventListener(name: string, callback: (event: BeforeUnload) => void) {
      listeners.get(name)?.delete(callback)
    },
    fetch(_input: unknown, init: RequestInit) {
      const request: Request = { headers: Promise.withResolvers<Response>(), signal: init.signal! }
      requests.push(request)
      request.signal.addEventListener('abort', () => {
        request.headers.reject(request.signal.reason)
        request.body?.error(request.signal.reason)
      }, { once: true })
      return request.headers.promise
    },
  }

  // Controlled message delivery shares this fixture's task queue, but is not
  // a production timer. The liveness case below uses actual Node message ports.
  class ControlledMessageChannel {
    port1: { onmessage: (() => void) | null, close: () => void }
    port2: { postMessage: (value: unknown) => void, close: () => void }

    constructor() {
      let closed = false
      let delivery: number | undefined
      const close = () => {
        closed = true
        if (delivery !== undefined)
          timers.delete(delivery)
      }

      this.port1 = { onmessage: null, close }
      this.port2 = {
        postMessage: () => {
          delivery = ++nextTimer
          timers.set(delivery, {
            delay: 0,
            callback: () => {
              if (!closed)
                this.port1.onmessage?.()
            },
          })
        },
        close,
      }
    }
  }

  window.top = window
  document.defaultView = window
  vi.stubGlobal('window', window)
  vi.stubGlobal('document', document)
  const sandbox = {
    window,
    document,
    AbortController: HostController,
    MessageChannel: nativePublication ? HostMessageChannel : ControlledMessageChannel,
    session,
    state,
    AvailabilityDeparture,
    // Call through to the real factory. Observe the exact returned JSON promise;
    // neither it nor its cause is replaced by this test adapter.
    readAvailability(signal: AbortSignal) {
      const read = readAvailability(signal)
      void read.response.then((response) => {
        const consume = response.json
        response.json = () => {
          const original = consume()
          bodies.push(original)
          void original.catch(() => {})
          return original
        }
      }, () => {})
      return read
    },
    shallowRef<T>(value: T) { return { value } },
    triggerRef() {},
    watch(_getter: unknown, callback: () => void) { watchers.push(callback) },
    onScopeDispose(callback: () => void) { disposals.push(callback) },
    redirect(value: string) { redirects.push(value) },
    setTimeout(callback: () => void, delay: number) {
      const id = ++nextTimer
      timers.set(id, { callback, delay })
      return id
    },
    clearTimeout(id: number) { timers.delete(id) },
    control: undefined as unknown as Control,
  }
  runInNewContext(`${lifetime}\n${polling}\ncontrol = { refreshAvailability, departDocument, snapshot: () => ({ failures: availabilityFailures, request: availabilityRequest, generation: documentLifetime.value }) }`, sandbox)
  const flush = async () => {
    await setImmediate()
  }

  const takeTask = (delay: number) => {
    const entry = [...timers].find(([, timer]) => timer.delay === delay)
    assert.ok(entry, `Expected one owned task with delay ${delay}`)
    timers.delete(entry[0])
    return entry[1].callback
  }

  const runTask = async (delay: number) => {
    takeTask(delay)()
    await flush()
  }

  const headers = (index: number, status = 200) => {
    const request = requests[index]
    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        request.body = controller
      },
    })
    request.headers.resolve(new HostResponse(stream, { status, headers: { 'Content-Type': 'application/json' } }))
  }

  const body = (index: number, text = JSON.stringify(online)) => {
    requests[index].body!.enqueue(new TextEncoder().encode(text))
    requests[index].body!.close()
  }

  const reply = (index: number) => {
    headers(index)
    body(index)
  }

  const failBody = async (index: number, cause: unknown) => {
    headers(index)
    await flush()
    requests[index].body!.error(cause)
    await flush()
  }

  const event = (name: 'beforeunload' | 'pagehide' | 'pageshow') => {
    const value = name === 'beforeunload' ? new BeforeUnload(document, window) : new PageTransition(document, window, name)
    for (const callback of [...listeners.get(name) || []])
      callback(value)
  }

  return {
    control: sandbox.control,
    document,
    published: published.promise,
    session,
    state,
    timers,
    requests,
    bodies,
    redirects,
    flush,
    takeTask,
    runTask,
    headers,
    body,
    reply,
    failBody,
    event,
    visibility() {
      for (const callback of documentListeners.get('visibilitychange') || [])
        callback()
    },
    notify() {
      for (const callback of watchers)
        callback()
    },
    dispose() {
      for (const callback of disposals)
        callback()
    },
    delays: () => [...timers.values()].map(timer => timer.delay),
    offlineWrites: () => offlineWrites,
  }
}

let current: ReturnType<typeof fixture>

beforeEach(() => {
  current = fixture()
})
afterEach(async () => {
  current.dispose()
  await current.flush()
  vi.unstubAllGlobals()
})

it('preserves the same original terminal without publishing after generation retirement while its caller remains alive', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  f.headers(0)
  await f.flush()
  const owner = f.control.snapshot()
  assert.ok(owner.request)
  assert.equal(owner.request.signal.aborted, false)
  f.control.departDocument()
  assert.equal(owner.generation.signal.aborted, true)
  assert.equal(owner.request.signal.aborted, false)
  const cause = new TypeError('original JSON transport failure after retirement')
  f.requests[0].body!.error(cause)
  await cycle
  await f.flush()
  await assert.rejects(f.bodies[0], error => error === cause)
  assert.equal(owner.request.signal.aborted, false)
  assert.deepEqual({
    offlineWrites: f.offlineWrites(),
    online: f.state.installationOnline,
    failures: f.control.snapshot().failures,
    tasks: f.delays(),
  }, { offlineWrites: 0, online: true, failures: 0, tasks: [] })
  assert.equal(f.control.snapshot().request, undefined)
})

it('keeps an identical original TypeError but cancels publication when pagehide precedes its task', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  const cause = new TypeError('original body failure')
  await f.failBody(0, cause)
  await cycle
  await assert.rejects(f.bodies[0], error => error === cause)
  assert.equal(f.state.installationOnline, true)
  assert.equal(f.control.snapshot().failures, 0)
  const queued = f.takeTask(0)
  f.event('pagehide')
  queued()
  await f.flush()
  assert.equal(f.offlineWrites(), 0)
  assert.equal(f.control.snapshot().failures, 0)
  assert.equal(f.control.snapshot().request, undefined)
  assert.equal(f.timers.size, 0)
})

for (const kind of ['network', 'http', 'json', 'body'] as const) {
  it(`publishes an active or retained-document ${kind} failure once, then recovers and rearms`, async () => {
    const f = current
    const cycle = f.control.refreshAvailability()
    const cause = new TypeError('original active failure')
    if (kind === 'network') {
      f.requests[0].headers.reject(cause)
    }
    else if (kind === 'http') {
      f.headers(0, 503)
    }
    else if (kind === 'json') {
      f.headers(0)
      f.body(0, '{invalid JSON')
    }
    else {
      await f.failBody(0, cause)
    }

    await f.flush()
    await cycle
    assert.equal(f.state.installationOnline, true)
    assert.equal(f.control.snapshot().failures, 0)
    assert.deepEqual(f.delays(), [0])
    // beforeunload alone is not evidence that this document will disappear.
    // A cancelled/download navigation keeps this publication and cadence alive.
    f.event('beforeunload')
    const publication = f.takeTask(0)
    publication()
    await f.flush()
    publication()
    await f.flush()
    assert.equal(f.offlineWrites(), 1)
    assert.equal(f.control.snapshot().failures, 1)
    assert.deepEqual(f.delays(), [6000])
    await f.runTask(6000)
    assert.equal(f.requests.length, 2)
    f.reply(1)
    await f.flush()
    assert.equal(f.state.installationOnline, true)
    assert.equal(f.control.snapshot().failures, 0)
    assert.deepEqual(f.delays(), [3000])
    await f.runTask(3000)
    assert.equal(f.requests.length, 3)
    f.requests[2].headers.reject(new TypeError('second real failure'))
    await f.flush()
    await f.runTask(0)
    assert.equal(f.control.snapshot().failures, 1)
    assert.deepEqual(f.delays(), [6000])
  })
}

it('publishes a real failure if its task runs before pagehide', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  const cause = new TypeError('published before departure')
  await f.failBody(0, cause)
  await cycle
  await f.runTask(0)
  assert.equal(f.state.installationOnline, false)
  assert.equal(f.control.snapshot().failures, 1)
  assert.equal(f.offlineWrites(), 1)
  f.event('pagehide')
  assert.equal(f.state.installationOnline, false)
  assert.equal(f.control.snapshot().failures, 1)
  assert.equal(f.timers.size, 0)
  await assert.rejects(f.bodies[0], error => error === cause)
})

it('refuses an old publication after hide then show and keeps the fresh generation polling', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  await f.failBody(0, new TypeError('old generation failure'))
  await cycle
  const queued = f.takeTask(0)
  const generation = f.control.snapshot().generation
  f.event('pagehide')
  f.event('pageshow')
  assert.equal(generation.signal.aborted, true)
  assert.notEqual(f.control.snapshot().generation, generation)
  assert.equal(f.requests.length, 2)
  f.reply(1)
  await f.flush()
  queued()
  await f.flush()
  assert.equal(f.state.installationOnline, true)
  assert.equal(f.control.snapshot().failures, 0)
  assert.equal(f.offlineWrites(), 0)
  assert.equal(f.control.snapshot().request, undefined)
  assert.deepEqual(f.delays(), [3000])
})

for (const cancellation of ['caller', 'session', 'signout', 'redirect', 'replacement', 'dispose', 'generation'] as const) {
  it(`settles pending publication ownership on ${cancellation} without stale writes or duplicate timers`, async () => {
    const f = current
    const cycle = f.control.refreshAvailability()
    await f.failBody(0, new TypeError('cancelled owner failure'))
    await cycle
    const queued = f.takeTask(0)
    if (cancellation === 'caller') {
      f.control.snapshot().request!.abort()
    }
    else if (cancellation === 'session') {
      f.session.value = account()
      f.notify()
    }
    else if (cancellation === 'signout' || cancellation === 'redirect') {
      f.state[cancellation === 'signout' ? 'signingOut' : 'redirecting'] = true
      f.notify()
    }
    else if (cancellation === 'replacement') {
      void f.control.refreshAvailability()
    }
    else if (cancellation === 'dispose') {
      f.dispose()
    }
    else {
      f.control.departDocument()
    }

    if (cancellation === 'session' || cancellation === 'replacement') {
      assert.equal(f.requests.length, 2)
      f.reply(1)
      await f.flush()
    }

    queued()
    await f.flush()
    assert.equal(f.state.installationOnline, true)
    assert.equal(f.offlineWrites(), 0)
    assert.equal(f.control.snapshot().failures, 0)
    assert.equal(f.control.snapshot().request, undefined)
    assert.deepEqual(f.delays(), cancellation === 'session' || cancellation === 'replacement' ? [3000] : [])
  })
}

it('does not reuse a dequeued poll after signout and replacement', async () => {
  const f = current
  void f.control.refreshAvailability()
  f.reply(0)
  await f.flush()
  const queued = f.takeTask(3000)
  f.state.signingOut = true
  f.notify()
  f.state.signingOut = false
  f.session.value = account()
  f.notify()
  assert.equal(f.requests.length, 2)
  f.reply(1)
  await f.flush()
  queued()
  await f.flush()
  assert.equal(f.requests.length, 2)
  assert.deepEqual(f.delays(), [3000])
})

it('keeps the active 401 redirect and stops every owned task', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  f.headers(0, 401)
  await cycle
  assert.equal(f.redirects.length, 1)
  assert.equal(f.timers.size, 0)
  f.event('pagehide')
  f.event('pageshow')
  assert.equal(f.requests.length, 1)
})

it('continues ordinary polling after beforeunload without a pagehide', async () => {
  const f = current
  const first = f.control.refreshAvailability()
  f.event('beforeunload')
  await first
  assert.equal(f.control.snapshot().generation.signal.aborted, false)
  assert.equal(f.offlineWrites(), 0)
  assert.deepEqual(f.delays(), [3000])
  await f.runTask(3000)
  assert.equal(f.requests.length, 2)
  const cause = new TypeError('real failure in retained document')
  await f.failBody(1, cause)
  await assert.rejects(f.bodies[0], error => error === cause)
  await f.runTask(0)
  assert.equal(f.offlineWrites(), 1)
  assert.deepEqual(f.delays(), [6000])
})

it('keeps consecutive active failures and fresh recovery on the existing backoff', async () => {
  const f = current
  void f.control.refreshAvailability()
  for (const [index, delay] of [6000, 12000, 15000].entries()) {
    f.requests[index].headers.reject(new TypeError('real active outage'))
    await f.flush()
    await f.runTask(0)
    assert.equal(f.control.snapshot().failures, index + 1)
    assert.deepEqual(f.delays(), [delay])
    await f.runTask(delay)
  }

  f.reply(3)
  await f.flush()
  assert.equal(f.control.snapshot().failures, 0)
  assert.equal(f.state.installationOnline, true)
  assert.deepEqual(f.delays(), [3000])
})

it('disposes a pending original JSON read with no publication or abandoned task', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  f.headers(0)
  await f.flush()
  f.dispose()
  await cycle
  assert.equal(f.requests[0].signal.aborted, true)
  assert.equal(f.control.snapshot().request, undefined)
  assert.equal(f.timers.size, 0)
  assert.equal(f.offlineWrites(), 0)
  assert.equal(f.control.snapshot().failures, 0)
})

it('publishes active failure independently of a paused timer clock', async () => {
  current.dispose()
  current = fixture(true)
  const f = current
  const cycle = f.control.refreshAvailability()
  const cause = new TypeError('real failure while only timers are paused')
  await f.failBody(0, cause)
  await cycle
  let deadline: ReturnType<typeof setNativeTimeout> | undefined
  try {
    await Promise.race([
      f.published,
      new Promise<never>((_, reject) => {
        deadline = setNativeTimeout(() => reject(new Error('Publication did not progress while timer clock is paused')), 1000)
      }),
    ])
  }
  finally {
    clearNativeTimeout(deadline)
  }

  assert.equal(f.offlineWrites(), 1)
  assert.equal(f.control.snapshot().failures, 1)
  assert.deepEqual(f.delays(), [6000])
  await assert.rejects(f.bodies[0], error => error === cause)
})

it('cancels authenticated session ownership immediately while hidden and resumes when visible', async () => {
  const f = current
  const cycle = f.control.refreshAvailability()
  await f.failBody(0, new TypeError('old hidden-session failure'))
  await cycle
  const owner = f.control.snapshot().request
  assert.ok(owner)
  f.document.hidden = true
  f.session.value = account()
  f.notify()
  assert.equal(owner.signal.aborted, true)
  assert.equal(f.control.snapshot().request, undefined)
  assert.equal(f.timers.size, 0)
  assert.equal(f.requests.length, 1)
  assert.equal(f.offlineWrites(), 0)
  f.document.hidden = false
  f.visibility()
  assert.equal(f.requests.length, 2)
  f.reply(1)
  await f.flush()
  assert.equal(f.state.installationOnline, true)
  assert.deepEqual(f.delays(), [3000])
})
