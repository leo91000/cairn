import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { execFileSync } from 'node:child_process'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import { EventEmitter } from 'node:events'
import { product, controlledSourceGitArgs } from './direct-owner-overlay-control.mjs'
import { observeDirectOwners } from './direct-owner-probe.mjs'
import { createContext, runInContext } from 'node:vm'
import { MessageChannel as NativeMessageChannel } from 'node:worker_threads'
import { setImmediate } from 'node:timers/promises'

const reference = process.argv.includes('--reference')
const productReferenceHead = process.env.C40_PRODUCT_REFERENCE || product
assert.match(productReferenceHead, /^[a-f0-9]{40}$/)
const referenceHead = process.env.C40_ARCHIVE_REFERENCE || '589e704e90bc451a181bd7fdcf667993e518522e'
function fixture(t, name, settings = {}) {
  const productReference = name === 'availability-probe.ts'
  const raw = productReference
    ? execFileSync('git', controlledSourceGitArgs(['show', `${productReferenceHead}:tests/e2e/availability-probe.ts`]), { encoding: 'utf8' })
    : reference
    ? execFileSync('git', controlledSourceGitArgs(['show', `${referenceHead}:.github/diagnostics/${name}`]), { encoding: 'utf8' })
    : readFileSync(new URL('./' + name, import.meta.url), 'utf8')
  const source = stripTypeScriptTypes(raw, { mode: 'transform' })
  let bootstrap
  if (name === 'availability-reader-native.mjs') {
    const start = source.indexOf('function nativeDiagnostics(mode) {')
    const end = source.indexOf('function ownPageCdp(', start)
    assert(start > 0 && end > start)
    bootstrap = source.slice(start, end).trim().replace('function nativeDiagnostics(mode)', 'function (mode)')
  }
  else {
    const start = source.indexOf('await page.addInitScript(') + 'await page.addInitScript('.length
    const next = source.indexOf(productReference ? 'export function availabilityBodiesAfter(' : name.startsWith('skill') ? 'await page.clock.install()' : 'await page.setViewportSize', start)
    assert(start > 30 && next > start)
    bootstrap = productReference ? source.slice(start, next).trim().replace(/\);\s*}$/, '') : source.slice(start, next).trim().replace(/\);$/, '')
  }
  const originals = new Map()
  const bodyCalls = []
  const originalAny = AbortSignal.any
  const originalAbort = AbortController.prototype.abort
  const abortCalls = []
  const abortResult = Symbol('original abort return stand-in')
  AbortController.prototype.abort = function (...args) {
    abortCalls.push({ receiver: this, args })
    if (settings.abortFailure)
      throw settings.abortFailure
    Reflect.apply(originalAbort, this, args)
    return abortResult
  }
  for (const method of ['json', 'text', 'arrayBuffer', 'blob', 'formData']) {
    const original = Response.prototype[method]
    originals.set(method, original)
    Response.prototype[method] = function (...args) {
      const promise = Reflect.apply(original, this, args)
      bodyCalls.push({ method, receiver: this, args, promise })
      return promise
    }
  }
  const channels = []
  let fixtureRetired = false
  t.after(() => {
    fixtureRetired = true
    AbortController.prototype.abort = originalAbort
    AbortSignal.any = originalAny
    for (const [method, original] of originals)
      Response.prototype[method] = original
    for (const channel of channels) {
      channel.port1.close()
      channel.port2.close()
    }
  })
  const queue = []
  const calls = []
  const events = []
  const closeResult = Symbol('actual original native-close stand-in result')
  class NativeSource extends EventTarget {
    static CLOSED = 2
    readyState = 0
    closes = 0
    constructor(url) { super(); this.url = String(url) }
    close() { this.readyState = 2; this.closes++; return closeResult }
  }
  const storage = new Map()
  const listeners = new Map()
  const directEvents = []
  const context = createContext({
    sessionStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) },
    cairnAvailabilityRecord: (...args) => { events.push(args); return Promise.resolve() },
    nativeStateReceipt: (...args) => { events.push(args); return Promise.resolve() },
    URL, Request, Response, AbortController, AbortSignal, DOMException, Error, TypeError, Object, Array, EventSource: NativeSource, crypto,
    Event, MessageEvent, Promise, performance,
    MessageChannel: class {
      constructor() {
        const channel = new NativeMessageChannel()
        channels.push(channel)
        if (settings.holdCompletionMessages)
          channel.port2.postMessage = () => {}
        if (fixtureRetired) {
          channel.port1.close()
          channel.port2.close()
        }
        return channel
      }
    },
    console: { error(...args) { settings.onFailureSignal?.(...args) } },
    fetch: function (...args) {
      calls.push({ receiver: this, args })
      const next = queue.shift()
      assert(next)
      return typeof next === 'function' ? Reflect.apply(next, this, args) : next
    },
    availabilityProbe: (...args) => { events.push(args); return Promise.resolve() },
    b534Record: (...args) => { events.push(args); return Promise.resolve() },
    __c40RecordDirectOwner: event => { directEvents.push(event); return Promise.resolve() },
    addEventListener(kind, callback) {
      if (!listeners.has(kind)) listeners.set(kind, new Set())
      listeners.get(kind).add(callback)
    },
    removeEventListener(kind, callback) { listeners.get(kind)?.delete(callback) },
    location: { href: 'http://localhost:4422/fixture', origin: 'http://localhost:4422' },
  })
  runInContext('window=globalThis;top=window;document={defaultView:window};', context)
  if (settings.localRealmData)
    runInContext('Object=({}).constructor;Array=[].constructor;', context)
  const unwrappedFetch = context.fetch
  if (settings.directOrder === 'direct-first')
    runInContext(`(${observeDirectOwners.toString()})({installationId:'source-id',installationSlug:'source-slug'})`, context)
  if (!settings.deferBootstrap)
    runInContext(`(${bootstrap})(${name === 'availability-reader-native.mjs' ? JSON.stringify('pre-pagehide-original-body') : ''});`, context)
  if (settings.directOrder === 'direct-last')
    runInContext(`(${observeDirectOwners.toString()})({installationId:'source-id',installationSlug:'source-slug'})`, context)
  const ownWindow = runInContext('window', context)
  const fetch = (...args) => Reflect.apply(context.fetch, ownWindow, args)
  return { context, fetch, unwrappedFetch, queue, calls, bodyCalls, events, closeResult, NativeSource, abortCalls, abortResult, storage, directEvents, bootstrapExpression: `(${bootstrap})(${name === 'availability-reader-native.mjs' ? JSON.stringify('pre-pagehide-original-body') : ''});` }
}
for (const name of ['skill-availability-probe.ts', 'b534-navigation-probe.ts']) {
  test(`${name}: actual bootstrap preserves original fetch promise, arguments and receiver`, async (t) => {
    const f = fixture(t, name)
    const original = Promise.withResolvers()
    f.queue.push(original.promise)
    const receiver = {}
    const options = { method: 'GET' }
    const result = Reflect.apply(f.context.fetch, receiver, ['/api/installations', options])
    original.resolve(Response.json({ value: 'original' }))
    await result
    assert.equal(result, original.promise)
    assert.equal(f.calls[0].receiver, receiver)
    assert.equal(f.calls[0].args[1], options)
  })
  test(`${name}: actual body observation returns the original native JSON promise`, async (t) => {
    const f = fixture(t, name)
    const response = Response.json({ value: 'original' })
    f.queue.push(Promise.resolve(response))
    const returned = await f.fetch('/api/installations', { method: 'GET' })
    const body = returned.json()
    await body
    assert.equal(body, f.bodyCalls.at(-1).promise)
    assert.equal(f.bodyCalls.at(-1).receiver, returned)
    assert.equal(Object.hasOwn(returned, 'json'), false)
  })
  test(`${name}: borrowing a body method keeps the ACTUAL receiver and original promise`, async (t) => {
    const f = fixture(t, name)
    const a = Response.json({ value: 'A' })
    const b = Response.json({ value: 'B' })
    f.queue.push(Promise.resolve(a), Promise.resolve(b))
    const first = await f.fetch('/api/installations', { method: 'GET' })
    const second = await f.fetch('/api/installations', { method: 'GET' })
    const body = Reflect.apply(first.json, second, [])
    assert.deepEqual(await body, { value: 'B' })
    assert.equal(f.bodyCalls.at(-1).receiver, second)
    assert.equal(body, f.bodyCalls.at(-1).promise)
    assert.equal(first.bodyUsed, false)
  })
}
test('B534 actual bootstrap source.close preserves actual receiver and original return', async (t) => {
  const f = fixture(t, 'b534-navigation-probe.ts')
  const first = new f.context.EventSource('/api/chats/stream')
  const second = new f.context.EventSource('/api/chats/stream')
  const returned = Reflect.apply(first.close, second, [])
  assert.equal(returned, f.closeResult)
  assert.equal(first.closes, 0)
  assert.equal(second.closes, 1)
  await setImmediate()
})


for (const name of ['skill-availability-probe.ts', 'b534-navigation-probe.ts']) {
  test(`H76 ${name}: malformed URL preserves the ORIGINAL rejected promise and cause`, async (t) => {
    const f = fixture(t, name)
    const cause = new TypeError('same native malformed URL rejection')
    const original = Promise.reject(cause)
    // Attach immediately so a synchronous observer throw cannot create an
    // unrelated unhandled-rejection error in the reference counterexample.
    void original.catch(() => {})
    f.queue.push(original)
    const result = f.fetch('http://[')
    assert.equal(result, original)
    await assert.rejects(result, value => value === cause)
    assert.equal(f.calls.length, 1)
  })
  test(`H76 ${name}: coercible inputs are consumed ONLY by original fetch`, async (t) => {
    const f = fixture(t, name)
    let reads = 0
    const input = { get url() { reads++; return '/api/installations' } }
    const original = Promise.resolve(Response.json({ value: 'original' }))
    f.queue.push((value) => { void value.url; return original })
    const result = f.fetch(input)
    assert.equal(result, original)
    await result
    assert.equal(reads, 1)
  })
  test(`H76 ${name}: accessor options are read ONLY by original fetch`, async (t) => {
    const f = fixture(t, name)
    let methodReads = 0
    let signalReads = 0
    const controller = new AbortController()
    const options = { get method() { methodReads++; return 'GET' }, get signal() { signalReads++; return controller.signal } }
    const original = Promise.resolve(Response.json({ value: 'original' }))
    f.queue.push((_, init) => { void init.method; void init.signal; return original })
    const result = f.fetch('/api/installations', options)
    assert.equal(result, original)
    await result
    assert.equal(methodReads, 1)
    assert.equal(signalReads, 1)
  })
}


for (const name of ['availability-reader-native.mjs', ...(process.argv.includes('--product-reference') ? ['availability-probe.ts'] : [])]) {
  test(`H77 ${name}: preserve actual empty original argument list`, async (t) => {
    const f = fixture(t, name)
    const original = Promise.resolve(Response.json({ value: 'original' }))
    f.queue.push(original)
    assert.equal(f.fetch(), original)
    await original
    assert.equal(f.calls[0].args.length, 0)
  })
  test(`H77 ${name}: original fetch owns input coercion and identical rejection`, async (t) => {
    const f = fixture(t, name)
    const cause = new TypeError('same original coercion failure')
    const original = Promise.reject(cause)
    void original.catch(() => {})
    const input = { toString() { throw cause } }
    f.queue.push(() => original)
    let returned
    assert.doesNotThrow(() => { returned = f.fetch(input) })
    assert.equal(returned, original)
    await assert.rejects(returned, value => value === cause)
    assert.equal(f.calls.length, 1)
  })
  test(`H77 ${name}: accessor-backed options not consumed by instrumentation`, async (t) => {
    const f = fixture(t, name)
    const original = Promise.resolve(Response.json({ value: 'original' }))
    let methodReads = 0
    let signalReads = 0
    const controller = new AbortController()
    const init = { get method() { methodReads++; return methodReads === 1 ? 'GET' : 'POST' }, get signal() { signalReads++; return controller.signal } }
    f.queue.push((_, options) => { void options.method; void options.signal; return original })
    assert.equal(f.fetch('/api/installations', init), original)
    await original
    assert.equal(methodReads, 1)
    assert.equal(signalReads, 1)
  })
}

test('H77 B534 native constructor resource/prototype survives unsupported coercible metadata', (t) => {
  const f = fixture(t, 'b534-navigation-probe.ts')
  let conversions = 0
  const input = { toString() { if (++conversions > 1) throw new TypeError('observer second conversion'); return '/api/chats/stream' } }
  let returned
  assert.doesNotThrow(() => { returned = new f.context.EventSource(input) })
  assert.equal(conversions, 1)
  assert.equal(Object.getPrototypeOf(returned), f.NativeSource.prototype)
  assert.equal(returned.readyState, 0)
})


if (process.argv.includes('--product-reference')) {
  for (const removesAccessor of [false, true]) {
    test(`R77 actual product probe refuses cross-option signal mutation (self-removing accessor ${removesAccessor})`, async (t) => {
      const f = fixture(t, 'availability-probe.ts')
      const actual = new AbortController()
      const unrelated = new AbortController()
      const original = Promise.withResolvers()
      const cause = new DOMException('same ORIGINAL native A failure', 'AbortError')
      let used
      let options
      options = {
        signal: actual.signal,
        get window() {
          options.signal = unrelated.signal
          if (removesAccessor)
            delete options.window
          return undefined
        },
      }
      // Model only the relevant native dictionary conversion order. The actual
      // observer bootstrap is executed unchanged; no helper logic is copied.
      f.queue.push((_, init) => {
        used = init.signal
        void init.window
        used.addEventListener('abort', () => original.reject(cause), { once: true })
        return original.promise
      })
      const returned = f.fetch('/api/installations', options)
      assert.equal(returned, original.promise)
      assert.equal(used, actual.signal)
      unrelated.abort(new DOMException('different B reason', 'AbortError'))
      const rejected = assert.rejects(returned, error => error === cause)
      actual.abort(cause)
      await rejected
      await setImmediate()
      const journal = f.events.map(args => args[0]).filter(value => value && typeof value === 'object')
      t.diagnostic(JSON.stringify({ nativeUsedA: used === actual.signal, observerFailures: journal.filter(event => event.kind === 'observer-failed').length, wrongOwnedRejectionCount: journal.filter(event => event.kind === 'rejected' && event.aborted === true).length }))
      assert.equal(journal.some(event => event.kind === 'observer-failed'), true)
      assert.equal(journal.some(event => event.kind === 'rejected' && event.aborted === true), false)
    })
  }
}


test('H78 archived ordinary abort preserves exact arguments and an original native result', (t) => {
  const f = fixture(t, 'availability-reader-native.mjs')
  for (const args of [[], [new DOMException('original reason', 'AbortError'), 'extra']]) {
    const controller = new f.context.AbortController()
    const returned = Reflect.apply(controller.abort, controller, args)
    assert.equal(returned, f.abortResult)
    assert.equal(f.abortCalls.at(-1).receiver, controller)
    assert.deepEqual(f.abortCalls.at(-1).args, args)
  }
})

test('H78 archived ordinary abort never reads an instance signal accessor before the original', (t) => {
  const f = fixture(t, 'availability-reader-native.mjs')
  const controller = new f.context.AbortController()
  const signal = controller.signal
  Object.defineProperty(controller, 'signal', { get() { throw new Error('observer-only instance getter refusal') } })
  let returned
  assert.doesNotThrow(() => { returned = controller.abort() })
  assert.equal(returned, f.abortResult)
  assert.equal(f.abortCalls.length, 1)
  assert.equal(signal.aborted, true)
})

test('H78 archived original JSON consumer retains exact arguments and native promise', async (t) => {
  const f = fixture(t, 'availability-reader-native.mjs')
  const response = Response.json({ original: true })
  f.queue.push(Promise.resolve(response))
  const returned = await f.fetch('/api/installations')
  const original = Reflect.apply(returned.json, returned, ['extra', undefined])
  assert.equal(original, f.bodyCalls.at(-1).promise)
  assert.deepEqual(f.bodyCalls.at(-1).args, ['extra', undefined])
  assert.equal(f.bodyCalls.at(-1).receiver, returned)
  assert.deepEqual(await original, { original: true })
})


if (process.argv.includes('--product-reference')) {
  for (const field of ['cache', 'method', 'signal']) {
    test(`R78 actual product probe refuses inherited ${field} conversion and descriptor-map access`, async (t) => {
      const f = fixture(t, 'availability-probe.ts')
      const actual = new AbortController()
      const unrelated = new AbortController()
      const original = Promise.withResolvers()
      const cause = new DOMException('same original actual request failure', 'AbortError')
      const options = field === 'signal' ? { method: 'GET' } : { signal: unrelated.signal }
      let used
      let nativeGetterReads = 0
      let metadataGetterReads = 0
      f.queue.push((_, init) => {
        void init.cache
        void init.method
        used = init.signal
        const listenerOptions = Object.create(null)
        listenerOptions.once = true
        used.addEventListener('abort', () => original.reject(cause), listenerOptions)
        return original.promise
      })
      const prior = Object.getOwnPropertyDescriptor(Object.prototype, field)
      Object.defineProperty(Object.prototype, field, { configurable: true, get() {
        if (this === options) {
          nativeGetterReads++
          if (field !== 'signal')
            options.signal = actual.signal
          return field === 'signal' ? actual.signal : field === 'method' ? 'GET' : 'default'
        }
        const marker = Object.getOwnPropertyDescriptor(this, field === 'signal' ? 'method' : 'signal')?.value
        if (marker && typeof marker === 'object' && Object.hasOwn(marker, 'value')) {
          metadataGetterReads++
          return { value: field === 'signal' ? unrelated.signal : 'GET' }
        }
        return undefined
      } })
      let returned
      try { returned = f.fetch('/api/installations', options) }
      finally {
        if (prior)
          Object.defineProperty(Object.prototype, field, prior)
        else
          Reflect.deleteProperty(Object.prototype, field)
      }
      assert.equal(returned, original.promise)
      assert.equal(used, actual.signal)
      unrelated.abort(new DOMException('different unrelated reason', 'AbortError'))
      const rejected = assert.rejects(returned, error => error === cause)
      actual.abort(cause)
      await rejected
      await setImmediate()
      const journal = f.events.map(args => args[0]).filter(value => value && typeof value === 'object')
      t.diagnostic(JSON.stringify({ nativeGetterReads, metadataGetterReads, wrongOwnedRejectionCount: journal.filter(event => event.kind === 'rejected' && event.aborted).length }))
      assert.equal(metadataGetterReads, 0)
      assert.equal(nativeGetterReads, 1)
      assert.equal(journal.some(event => event.kind === 'observer-failed'), true)
      assert.equal(journal.some(event => event.kind === 'rejected' && event.aborted), false)
    })
  }
}

for (const name of ['skill-availability-probe.ts', 'b534-navigation-probe.ts', 'availability-reader-native.mjs']) {
  test(`H79 ${name}: unsupported conversion cannot attach abort observation to another signal`, async (t) => {
    const f = fixture(t, name)
    const actual = new f.context.AbortController()
    const unrelated = new f.context.AbortController()
    const original = Promise.resolve(new Response(null, { status: 204 }))
    const reader = name === 'availability-reader-native.mjs'
    let options
    options = reader
      ? { signal: unrelated.signal, get method() { options.signal = actual.signal; return 'GET' } }
      : { method: 'GET', signal: actual.signal, get window() { options.signal = unrelated.signal; return undefined } }
    let used
    f.queue.push((_, init) => {
      if (reader) {
        void init.method
        used = init.signal
      }
      else {
        used = init.signal
        void init.window
      }
      return original
    })
    const returned = f.fetch('/api/installations', options)
    assert.equal(returned, original)
    assert.equal(used, actual.signal)
    unrelated.abort()
    await returned
    await setImmediate()
    if (reader) {
      const journal = JSON.parse(f.storage.get('cairn-native-fetch-journal') || '[]')
      assert.equal(journal.some(event => event.kind === 'observer-failed'), true)
      assert.equal(journal.some(event => event.kind === 'private-native-signal'), false)
    }
    else {
      assert.equal(f.context.shellObserverFailed, true)
      assert.equal(f.events.some(event => event[0] === 'poll-abort'), false)
    }
    assert.equal(actual.signal.aborted, false)
  })
}


async function descriptorGetCounterexample(t, sameReason) {
  const f = fixture(t, 'availability-probe.ts')
  const actual = new AbortController()
  const unrelated = new AbortController()
  const native = Promise.withResolvers()
  const cause = new DOMException('same original actual request rejection', 'AbortError')
  let nativeGetReads = 0
  const target = { signal: unrelated.signal }
  const options = new Proxy(target, { get(object, key, receiver) {
    if (key === 'signal') {
      nativeGetReads++
      return actual.signal
    }
    return Reflect.get(object, key, receiver)
  } })
  let used
  f.queue.push((_, init) => {
    used = init.signal
    used.addEventListener('abort', () => native.reject(cause), { once: true })
    return native.promise
  })
  const returned = f.fetch('/api/installations', options)
  assert.equal(returned, native.promise)
  assert.equal(used, actual.signal)
  unrelated.abort(sameReason ? cause : new DOMException('different unrelated reason', 'AbortError'))
  let originalCausePreserved = false
  const rejected = assert.rejects(returned, value => {
    originalCausePreserved = value === cause
    return originalCausePreserved
  })
  actual.abort(cause)
  await rejected
  await setImmediate()
  const journal = f.events.map(args => args[0]).filter(value => value && typeof value === 'object')
  return {
    sameReason,
    // Out-of-domain is known because THIS fixture constructs a Proxy. This is
    // never a proposed runtime classification algorithm.
    fixtureDomain: 'OUTSIDE_PINNED_PRODUCERS',
    nativeUsedA: used === actual.signal,
    nativeGetReads,
    descriptorReportedB: Object.getOwnPropertyDescriptor(target, 'signal').value === unrelated.signal,
    originalPromisePreserved: returned === native.promise,
    originalCausePreserved,
    wrongOwnedRejectionCount: journal.filter(event => event.kind === 'rejected' && event.aborted).length,
    observerFailureCount: f.context.cairnAvailabilityProbeFailures,
    declaredScope: journal.find(event => event.kind === 'rejected')?.ownershipScope ?? null,
  }
}

if (process.argv.includes('--proxy-counterexample')) {
  for (const sameReason of [false, true]) {
    test(`R79 UNIVERSAL CLAIM REMAINS RED: descriptor/Get (same reason ${sameReason})`, async (t) => {
      const evidence = await descriptorGetCounterexample(t, sameReason)
      t.diagnostic(JSON.stringify(evidence))
      assert.equal(evidence.wrongOwnedRejectionCount, 0)
      assert.equal(evidence.observerFailureCount > 0, true)
    })
  }
}

if (process.argv.includes('--source-trusted-limit')) {
  for (const sameReason of [false, true]) {
    test(`SOURCE LIMIT RECOGNITION ONLY, NOT OWNERSHIP PASS: descriptor/Get (same reason ${sameReason})`, async (t) => {
      const evidence = await descriptorGetCounterexample(t, sameReason)
      assert.equal(evidence.fixtureDomain, 'OUTSIDE_PINNED_PRODUCERS')
      assert.equal(evidence.originalPromisePreserved, true)
      assert.equal(evidence.originalCausePreserved, true)
      assert.equal(evidence.nativeUsedA, true)
      assert.equal(evidence.nativeGetReads, 1)
      assert.equal(evidence.descriptorReportedB, true)
      assert.equal(evidence.wrongOwnedRejectionCount, 1)
      assert.equal(evidence.observerFailureCount, 0)
      assert.equal(evidence.declaredScope, 'PINNED_SOURCE_CONDITIONAL')
      const receipt = { category: 'OUT_OF_DOMAIN_LIMIT_RECOGNITION', universalOwnershipClaimQualified: false, proxyDetectedAtRuntime: false, nativeQualified: false, historicalCounterexampleStillRed: true, ...evidence }
      t.diagnostic(JSON.stringify(receipt))
      if (process.env.C40_EVIDENCE_DIR)
        writeFileSync(join(process.env.C40_EVIDENCE_DIR, `proxy-limit-${sameReason ? 'same' : 'distinct'}-reason.json`), JSON.stringify(receipt))
    })
  }
}

for (const directOrder of ['direct-first', 'direct-last']) {
  test(`PINNED PRODUCER SOURCE SUPPORT: real readAvailability and complete probe composition (${directOrder})`, async (t) => {
    const f = fixture(t, 'availability-probe.ts', { directOrder, localRealmData: true })
    const factorySource = execFileSync('git', controlledSourceGitArgs(['show', `${productReferenceHead}:apps/web/src/availability-read.ts`]), { encoding: 'utf8' })
    const factory = stripTypeScriptTypes(factorySource, { mode: 'transform' }).replace(/^export /gm, '')
    runInContext(`${factory}\nwindow.sourceReadAvailability=readAvailability`, f.context)
    const active = Promise.withResolvers()
    f.queue.push(active.promise)
    const first = f.context.sourceReadAvailability(new f.context.AbortController().signal)
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0].args[0], '/api/installations')
    assert.equal(f.calls[0].args[1].credentials, 'same-origin')
    assert.equal(f.calls[0].args[1].keepalive, true)
    const cause = new TypeError('same active original factory fetch rejection')
    const rejected = assert.rejects(first.response, value => value === cause)
    active.reject(cause)
    await rejected
    const expected = [{ online: true }]
    f.context.cairnExpectedAvailability = expected
    const response = Response.json(expected)
    response.clone = () => { throw new Error('producer must not clone the original response') }
    f.queue.push(Promise.resolve(response))
    const next = f.context.sourceReadAvailability(new f.context.AbortController().signal)
    const headers = await next.response
    assert.deepEqual(await headers.json(), expected)
    await setImmediate()
    assert.equal(f.calls.length, 2)
    assert.equal(f.bodyCalls.length, 1)
    assert.equal(f.bodyCalls[0].receiver, response)
    const journal = JSON.parse(f.storage.get('cairn-e2e-availability-journal') || '[]')
    assert.equal(journal.some(event => event.kind === 'rejected' && event.errorName === 'TypeError' && !event.aborted), true)
    assert.equal(journal.filter(event => event.kind === 'body' && event.exactBody).length, 1)
    assert.equal(journal.every(event => event.ownershipScope === 'PINNED_SOURCE_CONDITIONAL'), true)
    assert.equal(f.context.cairnAvailabilityProbeFailures, 0)
    assert.equal(f.context.__c40DirectOwnerReceipt.snapshot().observerFailures, 0)
  })
}


for (const directOrder of [undefined, 'direct-first', 'direct-last']) {
  test(`R82 actual serialized bootstrap preserves strict unqualified caller receiver (${directOrder ?? 'availability-only'})`, async (t) => {
    const f = fixture(t, 'availability-probe.ts', { directOrder, localRealmData: true })
    const original = Promise.resolve(new Response(null, { status: 204 }))
    f.queue.push(original)
    const returned = runInContext(`(function(){'use strict';const request=window.fetch;return request('/api/installations/source-id/direct/source-connection/signal',{method:'POST'});})()`, f.context)
    assert.equal(returned, original)
    await returned
    assert.equal(f.calls.length, 1)
    assert.equal(f.calls[0].receiver, undefined)
    assert.equal(f.calls[0].args[0], '/api/installations/source-id/direct/source-connection/signal')
  })
}

test('H83 actual B534 ordinary controller retains native instance prototype and constructor', (t) => {
  const f = fixture(t, 'b534-navigation-probe.ts')
  const controller = new f.context.AbortController()
  assert.equal(Object.getPrototypeOf(controller), AbortController.prototype)
  assert.equal(controller.constructor, AbortController)
  assert.equal(controller.signal instanceof AbortSignal, true)
})


function oauthXhrFixture(settings = {}) {
  const raw = execFileSync('git', controlledSourceGitArgs(['show', `${productReferenceHead}:tests/e2e/mcps.spec.ts`]), { encoding: 'utf8' })
  const source = stripTypeScriptTypes(raw, { mode: 'transform' })
  const marker = 'await page.addInitScript('
  const testStart = source.indexOf('OAuth navigation does not leave background polls')
  const start = source.indexOf(marker, testStart) + marker.length
  const end = source.indexOf('}, pending ===', start) + 1
  assert(testStart >= 0 && start > testStart && end > start)
  const calls = []
  const openResult = Symbol('original open return SOURCE stand-in')
  const sendResult = Symbol('original send return SOURCE stand-in')
  class OriginalXhr {}
  OriginalXhr.prototype.open = function (...args) {
    calls.push({ method: 'open', receiver: this, args })
    if (settings.openFailure)
      throw settings.openFailure
    if (args.length >= 2)
      String(args[1]) // SOURCE stand-in for the original conversion, never native credit.
    return openResult
  }
  OriginalXhr.prototype.send = function (...args) {
    calls.push({ method: 'send', receiver: this, args })
    if (settings.sendFailure)
      throw settings.sendFailure
    return sendResult
  }
  const fetchCalls = []
  const probes = []
  const diagnosticTasks = []
  const fetchResult = settings.fetchResult ?? Promise.resolve('original fetch SOURCE result')
  const context = createContext({
    XMLHttpRequest: OriginalXhr, AbortSignal, AbortController, EventTarget,
    fetch: function (...args) {
      fetchCalls.push({ receiver: this, args })
      if (settings.fetchFailure)
        throw settings.fetchFailure
      if (settings.nativeSignalRead)
        void args[1]?.signal
      return fetchResult
    },
    queueMicrotask(callback) { diagnosticTasks.push(callback) },
    console: { error(...args) { settings.onDiagnosticSignal?.(...args) } },
    cairnOAuthProbe(value) { probes.push(value); if (settings.probeFailure) throw settings.probeFailure; return settings.probeResult ?? Promise.resolve() },
  })
  runInContext('globalThis.window = globalThis', context)
  runInContext(`(${source.slice(start, end)})(false)`, context)
  return { Xhr: OriginalXhr, calls, openResult, sendResult, context, fetchCalls, fetchResult, probes, diagnosticTasks, ownerWindow: runInContext('window', context), wrappedFetch: runInContext('window.fetch', context) }
}

test('H84 actual OAuth XHR bootstrap preserves real two-argument open and omitted send argument', () => {
  const f = oauthXhrFixture()
  const request = new f.Xhr()
  assert.equal(request.open('GET', '/api/installations'), f.openResult)
  assert.equal(request.send(), f.sendResult)
  assert.deepEqual(f.calls.map(value => value.args), [['GET', '/api/installations'], []])
  assert(f.calls.every(value => value.receiver === request))
})

test('H84 actual OAuth XHR bootstrap preserves all extra original arguments', () => {
  const f = oauthXhrFixture()
  const request = new f.Xhr()
  const extra = Symbol('extra original argument')
  const args = ['GET', '/api/installations', false, null, null, extra]
  assert.equal(Reflect.apply(request.open, request, args), f.openResult)
  assert.equal(Reflect.apply(request.send, request, ['body', extra]), f.sendResult)
  assert.deepEqual(f.calls.map(value => value.args), [args, ['body', extra]])
})

test('H84 actual OAuth XHR bootstrap adds no URL coercion to original conversion', () => {
  const f = oauthXhrFixture()
  let conversions = 0
  const url = { toString() { conversions++; return '/api/installations' } }
  const request = new f.Xhr()
  assert.equal(request.open('GET', url), f.openResult)
  assert.equal(conversions, 1)
  assert.equal(f.calls.length, 1)
  assert.equal(f.calls[0].args[1], url)
})

test('H84 actual OAuth XHR bootstrap preserves original conversion failure and native invocation count', () => {
  const f = oauthXhrFixture()
  const cause = new TypeError('original URL conversion SOURCE fixture')
  let conversions = 0
  const url = { toString() { conversions++; throw cause } }
  const request = new f.Xhr()
  assert.throws(() => request.open('GET', url), value => value === cause)
  assert.equal(conversions, 1)
  assert.equal(f.calls.length, 1)
})


function invokeOAuthFetch(f, receiver, args) {
  if (receiver === 'ownerWindow')
    return Reflect.apply(f.wrappedFetch, f.ownerWindow, args)
  f.context.sourceArgs = args
  return runInContext('(() => { "use strict"; const request = window.fetch; return request(...sourceArgs) })()', f.context)
}

for (const receiver of ['ownerWindow', 'unqualified']) {
  for (const terminal of ['success', 'rejection']) {
    test(`OAuth Fetch actual serialized bootstrap preserves ${receiver} receiver and original ${terminal}`, async () => {
      const cause = new TypeError('original Fetch cause SOURCE')
      const original = terminal === 'success' ? Promise.resolve('original body SOURCE') : Promise.reject(cause)
      const settlement = terminal === 'rejection' ? assert.rejects(original, error => error === cause) : original
      const f = oauthXhrFixture({ fetchResult: original })
      const options = { credentials: 'same-origin', keepalive: true, signal: new AbortController().signal }
      const args = terminal === 'success' ? ['/api/installations', options] : ['/api/installations', options, Symbol('extra original argument')]
      const result = invokeOAuthFetch(f, receiver, args)
      assert.equal(result, original)
      assert.equal(f.fetchCalls.length, 1)
      assert.equal(f.fetchCalls[0].receiver, receiver === 'ownerWindow' ? f.ownerWindow : undefined)
      assert.deepEqual(f.fetchCalls[0].args, args)
      if (terminal === 'rejection')
        await settlement
      else
        assert.equal(await result, 'original body SOURCE')
    })
  }
  test(`OAuth Fetch actual serialized bootstrap preserves ${receiver} original synchronous cause`, () => {
    const cause = new TypeError('original synchronous Fetch SOURCE')
    const f = oauthXhrFixture({ fetchFailure: cause })
    const args = ['/api/installations', { keepalive: true }]
    assert.throws(() => invokeOAuthFetch(f, receiver, args), error => error === cause)
    assert.equal(f.fetchCalls.length, 1)
    assert.equal(f.fetchCalls[0].receiver, receiver === 'ownerWindow' ? f.ownerWindow : undefined)
    assert.deepEqual(f.fetchCalls[0].args, args)
  })
}

test('OAuth Fetch actual serialized bootstrap adds no RequestInit or signal accessor evaluation', () => {
  const controller = new AbortController()
  let optionReads = 0
  let signalReads = 0
  Object.defineProperty(controller.signal, 'aborted', { get() { signalReads++; return false } })
  const options = { get signal() { optionReads++; return controller.signal } }
  const f = oauthXhrFixture({ nativeSignalRead: true })
  const result = invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', options])
  assert.equal(result, f.fetchResult)
  assert.equal(f.fetchCalls.length, 1)
  assert.equal(optionReads, 1)
  assert.equal(signalReads, 0)
  // Accessor inputs are outside pinned fresh literals: transparency only,
  // no ownership/producer admission is established by this witness.
})

for (const alreadyAborted of [false, true]) {
  test(`OAuth Fetch actual serialized departure audit retains abort notification (initially ${alreadyAborted})`, () => {
    const controller = new AbortController()
    if (alreadyAborted)
      controller.abort()
    const f = oauthXhrFixture()
    const options = { signal: controller.signal }
    assert.equal(invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', options]), f.fetchResult)
    if (!alreadyAborted)
      controller.abort()
    assert.equal(invokeOAuthFetch(f, 'ownerWindow', ['/api/installations', options]), f.fetchResult)
    assert.equal(f.fetchCalls.length, 2)
    assert.deepEqual(f.probes.map(value => value.kind), alreadyAborted ? ['background', 'background'] : ['background'])
  })
}


test('H85 actual nativeDiagnostics serialized Fetch preserves strict unqualified receiver and original promise', async (t) => {
  const f = fixture(t, 'availability-reader-native.mjs')
  const original = Promise.resolve(Response.json({ original: true }))
  const options = { method: 'GET' }
  f.queue.push(original)
  f.context.sourceArgs = ['/api/installations', options]
  const result = runInContext('(() => { "use strict"; const request = window.fetch; return request(...sourceArgs) })()', f.context)
  assert.equal(result, original)
  await result
  assert.equal(f.calls.length, 1)
  assert.equal(f.calls[0].receiver, undefined)
  assert.deepEqual(f.calls[0].args, f.context.sourceArgs)
})

test('H85 actual nativeDiagnostics serialized JSON preserves an unqualified original receiver and promise', async (t) => {
  const f = fixture(t, 'availability-reader-native.mjs')
  const method = f.context.Response.prototype.json
  const args = [Symbol('original extra JSON argument')]
  const result = Reflect.apply(method, undefined, args)
  // Invalid native receiver is a SOURCE transparency negative, not ownership.
  await assert.rejects(result)
  assert.equal(f.bodyCalls.length, 1)
  assert.equal(result, f.bodyCalls[0].promise)
  assert.equal(f.bodyCalls[0].receiver, undefined)
  assert.deepEqual(f.bodyCalls[0].args, args)
})

test('H85 actual nativeDiagnostics serialized abort preserves unqualified receiver and original throw', (t) => {
  const cause = new TypeError('original abort delegate SOURCE cause')
  const f = fixture(t, 'availability-reader-native.mjs', { abortFailure: cause })
  const method = f.context.AbortController.prototype.abort
  const args = [Symbol('original reason')]
  assert.throws(() => Reflect.apply(method, undefined, args), error => error === cause)
  assert.equal(f.abortCalls.length, 1)
  assert.equal(f.abortCalls[0].receiver, undefined)
  assert.deepEqual(f.abortCalls[0].args, args)
})


for (const terminal of ['success', 'rejection']) {
  test(`H87 actual OAuth metadata failure preserves original Fetch ${terminal} and becomes diagnostic failure`, async () => {
    const originalCause = new TypeError('original Fetch terminal SOURCE')
    const original = terminal === 'success' ? Promise.resolve('original SOURCE result') : Promise.reject(originalCause)
    const settled = terminal === 'rejection' ? assert.rejects(original, error => error === originalCause) : original
    const f = oauthXhrFixture({ fetchResult: original, nativeSignalRead: true })
    const metadataCause = new Error('descriptor observation SOURCE failure')
    const options = new Proxy({ signal: new AbortController().signal }, {
      get(target, key, receiver) { return Reflect.get(target, key, receiver) },
      getOwnPropertyDescriptor() { throw metadataCause },
    })
    let result
    assert.doesNotThrow(() => { result = invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', options]) })
    assert.equal(result, original)
    assert.equal(f.fetchCalls.length, 1)
    assert.equal(f.fetchCalls[0].receiver, f.ownerWindow)
    assert.equal(f.fetchCalls[0].args[1], options)
    assert.deepEqual(f.probes.map(value => value.kind), ['observer-error'])
    await settled
    // Proxy is OUTSIDE producer membership; this checks transparency, not admission.
  })
}

test('H87 actual OAuth diagnostic reporter failure cannot replace the original Promise', () => {
  const reportCause = new Error('original diagnostic reporter SOURCE failure')
  const f = oauthXhrFixture({ probeFailure: reportCause })
  const options = new Proxy({}, { getOwnPropertyDescriptor() { throw new Error('metadata SOURCE failure') } })
  let result
  assert.doesNotThrow(() => { result = invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', options]) })
  assert.equal(result, f.fetchResult)
  assert.equal(f.fetchCalls.length, 1)
  assert.equal(f.diagnosticTasks.length, 1)
  assert.throws(() => f.diagnosticTasks[0](), error => error === reportCause)
})


test('R87 actual OAuth XHR reporter cannot replace original successful send', () => {
  const reportCause = new Error('original XHR reporter SOURCE failure')
  const f = oauthXhrFixture({ probeFailure: reportCause })
  const control = new AbortController()
  invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', { signal: control.signal }])
  control.abort()
  const request = new f.Xhr()
  request.open('GET', '/api/installations')
  let result
  assert.doesNotThrow(() => { result = request.send() })
  assert.equal(result, f.sendResult)
  const sends = f.calls.filter(value => value.method === 'send')
  assert.equal(sends.length, 1)
  assert.equal(sends[0].receiver, request)
  assert.deepEqual(sends[0].args, [])
  assert.deepEqual(f.probes.map(value => value.kind), ['background', 'observer-error'])
  assert.equal(f.diagnosticTasks.length, 1)
  assert.throws(() => f.diagnosticTasks[0](), error => error === reportCause)
})

test('R87 actual OAuth XHR original synchronous send failure remains original', () => {
  const cause = new TypeError('original XHR send SOURCE cause')
  const f = oauthXhrFixture({ sendFailure: cause, probeFailure: new Error('reporter must not run') })
  const control = new AbortController()
  invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', { signal: control.signal }])
  control.abort()
  const request = new f.Xhr()
  request.open('GET', '/api/installations')
  assert.throws(() => request.send('original body'), error => error === cause)
  const sends = f.calls.filter(value => value.method === 'send')
  assert.equal(sends.length, 1)
  assert.deepEqual(sends[0].args, ['original body'])
  assert.equal(f.probes.length, 0)
  assert.equal(f.diagnosticTasks.length, 0)
})


test('R88 actual availability helper retains dual-sink failure in host audits after document replacement', async (t) => {
  const f = fixture(t, 'availability-probe.ts', { deferBootstrap: true })
  const source = stripTypeScriptTypes(execFileSync('git', controlledSourceGitArgs(['show', `${productReferenceHead}:tests/e2e/availability-probe.ts`]), { encoding: 'utf8' }), { mode: 'transform' }).replace(/^export /gm, '')
  runInContext(source, f.context)
  const page = new EventEmitter()
  const reads = []
  let hostFailures = 0
  let failSinks = false
  let bootstrap
  page.exposeFunction = async (name, callback) => {
    f.context[name] = event => failSinks ? Promise.reject(new Error('controlled binding refusal')) : Promise.resolve(callback(event))
  }
  page.addInitScript = async (callback) => {
    bootstrap = callback.toString()
    runInContext(`(${bootstrap})()`, f.context)
  }
  f.context.console = { error(...args) {
    assert.equal(args.length, 1)
    page.emit('console', { type: () => 'error', text: () => args[0] })
  } }
  const setItem = f.context.sessionStorage.setItem
  f.context.sessionStorage.setItem = (key, value) => {
    if (failSinks)
      throw new Error('controlled storage refusal')
    return setItem(key, value)
  }
  await f.context.installAvailabilityProbe(page, event => reads.push(event), () => { hostFailures++ })
  const read = async (value) => {
    const original = Promise.resolve(Response.json(value))
    f.queue.push(original)
    const result = f.fetch('/api/installations', { method: 'GET' })
    assert.equal(result, original)
    const response = await result
    const body = response.json()
    assert.equal(body, f.bodyCalls.at(-1).promise)
    assert.deepEqual(await body, value)
    await setImmediate()
  }
  await read({ complete: 'before' })
  assert.deepEqual(Array.from(f.context.incompleteAvailabilityReads(reads)), [])
  const readCountBefore = reads.length
  page.emit('console', { type: () => 'error', text: () => 'UNRELATED_SOURCE_LOG_MUST_NOT_BE_COLLECTED' })
  assert.equal(hostFailures, 0)
  assert.equal(reads.length, readCountBefore)
  failSinks = true
  await read({ original: 'lost only to observation' })
  assert.equal(f.context.cairnAvailabilityProbeFailures > 0, true)
  failSinks = false
  runInContext('document={defaultView:window}', f.context)
  runInContext(`(${bootstrap})()`, f.context)
  assert.equal(f.context.cairnAvailabilityProbeFailures, 0)
  await read({ complete: 'after replacement' })
  // The complete read census gap is genuinely invisible without host failure:
  // this invokes the actual audit, not a rewritten checker or synthetic terminal.
  assert.deepEqual(Array.from(f.context.incompleteAvailabilityReads(reads)), [])
  assert.equal(hostFailures > 0, true)
  assert.equal(reads.some(event => event.kind === 'observer-failed'), false)
  assert.throws(() => f.context.incompleteAvailabilityReads(reads, hostFailures), /observation failure|observer failure/i)
  page.emit('close')
  assert.equal(page.listenerCount('console'), 0)
})


function actualArchiveHostGuard(name) {
  const raw = readFileSync(new URL('./' + name, import.meta.url), 'utf8')
  const source = stripTypeScriptTypes(raw, { mode: 'transform' })
  const declaration = (name) => {
    const start = source.indexOf('const ' + name + ' = ')
    const open = source.indexOf('{', start)
    const end = source.indexOf('}', open) + 1
    assert(start >= 0 && open > start && end > open)
    return source.slice(start, end)
  }
  assert(source.includes('assertArchiveObserver()'))
  return runInContext(`(() => {let hostObserverFailures = 0;${declaration('archiveFailureListener')};${declaration('assertArchiveObserver')};return {accept:archiveFailureListener,count:()=>hostObserverFailures,audit:assertArchiveObserver}})()`, createContext({ Error }))
}

for (const name of ['skill-availability-probe.ts', 'b534-navigation-probe.ts', 'availability-reader-native.mjs']) {
  test(`R89 actual ${name} retains failed reporting in its real host guard after source replacement`, async (t) => {
    const host = actualArchiveHostGuard(name)
    assert.doesNotThrow(() => host.audit())
    const f = fixture(t, name, { holdCompletionMessages: true, onFailureSignal: (...args) => {
      assert.equal(args.length, 1)
      host.accept({ type: () => 'error', text: () => args[0] })
    } })
    let restore
    if (name === 'availability-reader-native.mjs') {
      const store = f.context.sessionStorage.setItem
      f.context.sessionStorage.setItem = (key, value) => {
        if (key === 'cairn-native-fetch-journal')
          throw new Error('controlled native observation journal refusal')
        return store(key, value)
      }
      restore = () => { f.context.sessionStorage.setItem = store }
    }
    else {
      const binding = name.startsWith('skill') ? 'availabilityProbe' : 'b534Record'
      const original = f.context[binding]
      f.context[binding] = () => Promise.reject(new Error('controlled archived binding delivery failure'))
      restore = () => { f.context[binding] = original }
    }
    const original = Promise.resolve(Response.json({ original: 'precise SOURCE body' }))
    f.queue.push(original)
    const nativeControl = name === 'availability-reader-native.mjs' ? new f.context.AbortController() : undefined
    const result = f.fetch('/api/installations', { method: 'GET', ...(nativeControl ? { signal: nativeControl.signal } : {}) })
    assert.equal(result, original)
    const response = await result
    const body = response.json()
    assert.equal(body, f.bodyCalls.at(-1).promise)
    assert.deepEqual(await body, { original: 'precise SOURCE body' })
    nativeControl?.abort()
    await setImmediate()
    if (name !== 'availability-reader-native.mjs')
      assert.equal(f.context.shellObserverFailed, true)
    if (typeof f.context.cairnNativePagehideEntry === 'function')
      f.context.cairnNativePagehideEntry()
    restore()
    // New bootstrap/document adapter, not a native navigation acquisition.
    runInContext('document={defaultView:window}', f.context)
    runInContext(f.bootstrapExpression, f.context)
    if (typeof f.context.cairnNativePagehideEntry === 'function')
      f.context.cairnNativePagehideEntry()
    if (name !== 'availability-reader-native.mjs')
      assert.equal(f.context.shellObserverFailed, false)
    assert.equal(host.count() > 0, true)
    assert.throws(() => host.audit(), /UNKNOWN/)
  })
}


{
  const universalClaim = process.argv.includes('--reflection-side-effect-counterexample')
  for (const name of ['availability-probe.ts', 'availability-reader-native.mjs', 'skill-availability-probe.ts', 'b534-navigation-probe.ts']) {
    test(`${universalClaim ? 'R90 UNIVERSAL TRANSPARENCY COUNTEREXAMPLE' : 'OUT_OF_DOMAIN_OPERATION_LIMIT_RECOGNITION'}: reflective Proxy trap changes native entry (${name})`, async (t) => {
      const f = fixture(t, name)
      const makeOptions = () => {
        const controller = new AbortController()
        const cause = new DOMException('observer-only reflection aborted SOURCE request', 'AbortError')
        const options = new Proxy({ method: 'GET', signal: controller.signal }, {
          getPrototypeOf() { controller.abort(cause); return Object.prototype },
        })
        return { controller, cause, options }
      }
      const nativeOperation = (_input, options) => options.signal.aborted ? Promise.reject(options.signal.reason) : Promise.resolve(Response.json({ original: true }))
      const baseline = makeOptions()
      f.queue.push(nativeOperation)
      const unobserved = Reflect.apply(f.unwrappedFetch, runInContext('window', f.context), ['/api/installations', baseline.options])
      await unobserved
      assert.equal(baseline.controller.signal.aborted, false)
      const observed = makeOptions()
      f.queue.push(nativeOperation)
      const result = f.fetch('/api/installations', observed.options)
      const outcome = await result.then(() => ({ aborted: false }), cause => ({ aborted: true, sameCause: cause === observed.cause }))
      t.diagnostic(JSON.stringify({ category: universalClaim ? 'UNIVERSAL_OPERATION_COUNTEREXAMPLE' : 'OUT_OF_DOMAIN_OPERATION_LIMIT_RECOGNITION', universalOperationEquivalenceQualified: false, inputDomain: 'OUTSIDE_PINNED_PRODUCERS', baselineEnteredUnaborted: true, observedEnteredAborted: observed.controller.signal.aborted, originalDelegateCalls: f.calls.length, nativeBrowserExecuted: false }))
      assert.equal(f.calls.length, 2)
      assert.equal(outcome.sameCause, true)
      assert.equal(outcome.aborted, !universalClaim, 'Universal equivalence is RED; recognition preserves the demonstrated out-of-domain interference')
    })
  }
}


test('H91 actual OAuth asynchronous reporting rejection remains host-owned after source departure', async () => {
  const cause = new Error('original asynchronous reporting SOURCE rejection')
  const rejected = Promise.reject(cause)
  const observedRejection = assert.rejects(rejected, error => error === cause)
  let hostFailures = 0
  const f = oauthXhrFixture({ probeResult: rejected, onDiagnosticSignal: (...args) => {
    assert.deepEqual(args, ['__c40_oauth_observer_failure_v1__'])
    hostFailures++
  } })
  const control = new AbortController()
  control.abort()
  const result = invokeOAuthFetch(f, 'ownerWindow', ['/api/installations/source/direct/authorize', { signal: control.signal }])
  assert.equal(result, f.fetchResult)
  assert.equal(f.fetchCalls.length, 1)
  await observedRejection
  await setImmediate()
  // The retained owner is outside the disappearing bootstrap closure.
  assert.equal(hostFailures > 0, true)
  assert.equal(f.probes.some(probe => probe.kind === 'observer-error'), true)
  assert.equal(f.diagnosticTasks.length > 0, true)
  assert.throws(() => f.diagnosticTasks[0](), error => error === cause)
})

// The actual driver function composes both serialized observers in its declared order.
// The optional historical helper is a permanent SOURCE counterfactual, not an old run relabelled.
async function nativeReaderCompositionFixture(t) {
  const name = 'availability-reader-native.mjs'
  const raw = readFileSync(new URL('./' + name, import.meta.url), 'utf8')
  const bindingStart = raw.indexOf('const nativeReaderCompositionBinding = ') + 'const nativeReaderCompositionBinding = '.length
  const bindingEnd = raw.indexOf('\nconst helperBinding = ', bindingStart)
  assert(bindingStart > 40 && bindingEnd > bindingStart)
  const binding = JSON.parse(raw.slice(bindingStart, bindingEnd))
  assert.equal(binding.compositionId, 'H91_NATIVE_READER_COMPOSITION_V4_SCOPED_GIT_ACQUISITION')
  assert.deepEqual(binding.order, ['nativeDiagnostics', 'installAvailabilityProbe'])
  const selected = process.env.C40_NATIVE_READER_HELPER_REFERENCE === 'historical' ? binding.historicalHelper : binding.helper
  const bytes = execFileSync('git', controlledSourceGitArgs(['show', `${selected.commit}:${selected.path}`]))
  assert.equal(execFileSync('git', controlledSourceGitArgs(['rev-parse', `${selected.commit}:${selected.path}`]), { encoding: 'utf8' }).trim(), selected.gitBlob)
  const { createHash } = await import('node:crypto')
  assert.equal(createHash('sha256').update(bytes).digest('hex'), selected.sha256)
  const f = fixture(t, name, { deferBootstrap: true })
  runInContext(stripTypeScriptTypes(bytes.toString(), { mode: 'transform' }).replace(/^export /gm, ''), f.context)
  const diagnosticFunction = f.bootstrapExpression.slice(0, f.bootstrapExpression.lastIndexOf(')("pre-pagehide-original-body");') + 1)
  f.context.nativeDiagnostics = runInContext(diagnosticFunction, f.context)
  f.context.assert = assert
  const start = raw.indexOf('async function installNativeReaderComposition(')
  const end = raw.indexOf('\nconst pollStart = ', start)
  assert(start > 0 && end > start)
  runInContext(raw.slice(start, end), f.context)
  const page = new EventEmitter()
  const reads = []
  let failures = 0
  page.exposeFunction = async (key, callback) => { f.context[key] = (...args) => Promise.resolve(callback(...args)) }
  page.addInitScript = async (script) => { runInContext(script, f.context) }
  f.context.console = { error(...args) { page.emit('console', { type: () => 'error', text: () => args[0] }) } }
  await f.context.installNativeReaderComposition(page, event => reads.push(event), () => { failures++ }, 'pre-pagehide-original-body')
  t.after(() => page.emit('close'))
  return { ...f, page, reads, failures: () => failures, binding, selected }
}

test('H91 ENTIRE NativeReader composition: strict unqualified receiver and empty full argument list', async (t) => {
  const f = await nativeReaderCompositionFixture(t)
  const original = Promise.resolve(Response.json({ original: true }))
  f.queue.push(original)
  const result = Reflect.apply(f.context.fetch, undefined, [])
  assert.equal(result, original)
  await result
  assert.equal(f.calls.length, 1)
  assert.equal(f.calls[0].receiver, undefined)
  assert.deepEqual(f.calls[0].args, [])
})

test('H91 ENTIRE NativeReader composition: ownerWindow full arguments and original rejected terminal', async (t) => {
  const f = await nativeReaderCompositionFixture(t)
  const cause = new TypeError('same ORIGINAL composition SOURCE failure')
  const original = Promise.reject(cause)
  const settled = assert.rejects(original, error => error === cause)
  const options = { method: 'GET', signal: new AbortController().signal }
  const args = ['/api/installations', options, Symbol('original extra argument')]
  f.queue.push(original)
  const result = f.fetch(...args)
  assert.equal(result, original)
  await settled
  assert.equal(f.calls.length, 1)
  assert.equal(f.calls[0].receiver, runInContext('window', f.context))
  assert.deepEqual(f.calls[0].args, args)
})

test('H91 ENTIRE NativeReader composition: original JSON receiver full arguments and promise', async (t) => {
  const f = await nativeReaderCompositionFixture(t)
  const response = Response.json({ original: true })
  f.queue.push(Promise.resolve(response))
  await f.fetch('/api/installations', { method: 'GET' })
  const args = [Symbol('original JSON extra argument')]
  const result = Reflect.apply(response.json, response, args)
  assert.deepEqual(await result, { original: true })
  assert.equal(f.bodyCalls.at(-1).receiver, response)
  assert.deepEqual(f.bodyCalls.at(-1).args, args)
  assert.equal(f.bodyCalls.at(-1).promise, result)
})

test('H91 ENTIRE NativeReader composition: original abort actual receiver full arguments and cause', async (t) => {
  const f = await nativeReaderCompositionFixture(t)
  const controller = new AbortController()
  const reason = new Error('original abort SOURCE cause')
  const args = [reason, Symbol('original abort extra argument')]
  const result = Reflect.apply(AbortController.prototype.abort, controller, args)
  assert.equal(result, f.abortResult)
  assert.equal(f.abortCalls.length, 1)
  assert.equal(f.abortCalls[0].receiver, controller)
  assert.deepEqual(f.abortCalls[0].args, args)
  assert.equal(controller.signal.reason, reason)
})

// Execute the REAL NativeReader host acquisition fragment, without importing its
// SDK/browser main. Git is real; only the child environment enforces ownership.
test('GIT OWNERSHIP actual NativeReader host helper acquisition across the repository boundary', (t) => {
  mkdirSync('test-results/source-only-native-reader-acquisition', { recursive: true })
  const directory = mkdtempSync(join(process.cwd(), 'test-results/source-only-native-reader-acquisition/fixture-'))
  t.after(() => rmSync(directory, { recursive: true, force: true }))
  const config = join(directory, 'empty-private-global-config')
  writeFileSync(config, '')
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: config, GIT_CONFIG_COUNT: '0', GIT_TEST_ASSUME_DIFFERENT_OWNER: '1' }
  delete env.GIT_CONFIG_PARAMETERS
  const reference = process.env.C40_NATIVE_READER_GIT_REFERENCE
  if (reference)
    assert.match(reference, /^[a-f0-9]{40}$/)
  const raw = reference
    ? execFileSync('git', controlledSourceGitArgs(['show', `${reference}:.github/diagnostics/availability-reader-native.mjs`]), { encoding: 'utf8' })
    : readFileSync(new URL('./availability-reader-native.mjs', import.meta.url), 'utf8')
  const start = raw.indexOf('const nativeReaderCompositionBinding = ') + 'const nativeReaderCompositionBinding = '.length
  const end = raw.indexOf('\nconst helperBinding = ', start)
  assert(start > 40 && end > start)
  const binding = JSON.parse(raw.slice(start, end))
  const fragmentEnd = raw.indexOf('\nconst sourceHashes = ', end)
  assert(fragmentEnd > end)
  const fragment = raw.slice(end, fragmentEnd)
  const source = {}
  let calls = 0
  let failure
  try {
    runInContext(fragment, createContext({
      nativeReaderCompositionBinding: binding, sources: source, assert,
      controlledSourceGitArgs,
      execFileSync(command, args, options = {}) {
        assert.equal(command, 'git')
        calls++
        return execFileSync(command, args, { ...options, cwd: process.cwd(), env })
      },
    }))
  }
  catch (error) { failure = error }
  t.diagnostic(JSON.stringify({ sourceReference: reference ?? 'CURRENT_SOURCE', actualGitAcquisitions: calls, gitFailureStatus: failure?.status ?? null, ownershipGuardEnforced: true, actualUidChanged: false, nativeExecuted: false }))
  assert.equal(failure, undefined, 'The real bound helper acquisitions must succeed only with exact repository trust')
  assert.equal(calls, 2)
  assert.equal(source.probe, execFileSync('git', controlledSourceGitArgs(['show', `${binding.helper.commit}:${binding.helper.path}`]), { encoding: 'utf8' }))
  assert.equal(readFileSync(config, 'utf8'), '')
  assert.equal(env.GIT_CONFIG_COUNT, '0')
})
