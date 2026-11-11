import assert from 'node:assert/strict'
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { execFileSync } from 'node:child_process'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import { product } from './direct-owner-overlay-control.mjs'
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
    ? execFileSync('git', ['show', `${productReferenceHead}:tests/e2e/availability-probe.ts`], { encoding: 'utf8' })
    : reference
    ? execFileSync('git', ['-c', `safe.directory=${process.cwd()}`, 'show', `${referenceHead}:.github/diagnostics/${name}`], { encoding: 'utf8' })
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
  t.after(() => {
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
        return channel
      }
    },
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
  if (settings.directOrder === 'direct-first')
    runInContext(`(${observeDirectOwners.toString()})({installationId:'source-id',installationSlug:'source-slug'})`, context)
  runInContext(`(${bootstrap})(${name === 'availability-reader-native.mjs' ? JSON.stringify('pre-pagehide-original-body') : ''});`, context)
  if (settings.directOrder === 'direct-last')
    runInContext(`(${observeDirectOwners.toString()})({installationId:'source-id',installationSlug:'source-slug'})`, context)
  const ownWindow = runInContext('window', context)
  const fetch = (...args) => Reflect.apply(context.fetch, ownWindow, args)
  return { context, fetch, queue, calls, bodyCalls, events, closeResult, NativeSource, abortCalls, abortResult, storage, directEvents }
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
    const factorySource = execFileSync('git', ['show', `${productReferenceHead}:apps/web/src/availability-read.ts`], { encoding: 'utf8' })
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
