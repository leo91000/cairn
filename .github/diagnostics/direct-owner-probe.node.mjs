import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { execFileSync } from 'node:child_process'
import path from 'node:path'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import { setImmediate } from 'node:timers/promises'
import { createContext, runInContext, runInNewContext } from 'node:vm'
import { applyTestOverlays, restoreTestOverlays, product } from './direct-owner-overlay-control.mjs'
import { observeDirectOwners, projectDirectOwners, createDirectOwnerObserver } from './direct-owner-probe.mjs'

function fixture(t) {
  const originalAny = AbortSignal.any
  const originalAbort = AbortController.prototype.abort
  const originalJson = Response.prototype.json
  const originalJsonPromises = new WeakMap()
  Response.prototype.json = function (...args) {
    const result = Reflect.apply(originalJson, this, args)
    originalJsonPromises.set(this, result)
    return result
  }
  t.after(() => {
    AbortSignal.any = originalAny
    AbortController.prototype.abort = originalAbort
    Response.prototype.json = originalJson
  })
  const callbacks = new Map()
  const storage = new Map()
  const ingresses = []
  const requests = []
  const queue = []
  const closeResult = Symbol('original close result')
  class Source extends EventTarget {
    static CONNECTING = 0
    static OPEN = 1
    static CLOSED = 2
    readyState = 0
    closes = 0
    constructor(url) { super(); this.url = url }
    close() { this.readyState = 2; this.closes++; return closeResult }
  }
  const nativeFetch = function (...args) {
    requests.push({ receiver: this, args })
    const next = queue.shift()
    if (!next)
      throw new Error('controlled source fixture has no queued original')
    if (next.throw)
      throw next.throw
    return next.promise
  }
  const context = createContext({
    AbortController, AbortSignal, Response, Event, EventSource: Source, Error,
    DOMException, URL, Map, WeakMap, Proxy, Promise, performance,
    BeforeUnloadEvent: class extends Event {}, PageTransitionEvent: class extends Event {},
    sessionStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => storage.set(key, value) },
    fetch: nativeFetch,
    __c40RecordDirectOwner: event => { ingresses.push(event); return Promise.resolve() },
    addEventListener: (name, callback) => { callbacks.set(name, callback) },
  })
  runInContext("window=globalThis; top=window; document={defaultView:window}; location={href:'http://localhost:4422/installations/private/agents',origin:'http://localhost:4422',pathname:'/installations/private/agents'}; cairnAvailabilityDocument='PRIVATE_DOCUMENT_CANARY'", context)
  runInContext(`(${observeDirectOwners.toString()})()`, context)
  const generation = new AbortController()
  const control = new AbortController()
  const signal = () => context.AbortSignal.any([generation.signal, control.signal])
  return {
    context, Source, generation, control, signal, queue, requests, storage, ingresses,
    closeResult, originalJsonPromises, callbacks,
    snapshot: () => context.__c40DirectOwnerReceipt.snapshot(),
    async authorize() {
      const native = Promise.withResolvers()
      queue.push(native)
      const options = { method: 'POST', signal: signal() }
      const returned = context.fetch('/api/installations/private/direct/authorize', options)
      assert.equal(returned, native.promise)
      native.resolve(Response.json({ available: true, grant: { claims: { connection_id: 'PRIVATE_CONNECTION_CANARY', token: 'NEVER_PUBLISH' } } }))
      const response = await returned
      const body = response.json()
      assert.equal(body, originalJsonPromises.get(response))
      await body
      await setImmediate()
      return new context.EventSource('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/events')
    },
  }
}
const privateErrors = [
  { message: '/localhost:4422/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal due to access control checks.' },
  { message: '/localhost:4422/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/events due to access control checks.' },
]

test('actual serialized wrappers preserve original this, fetch/json promises, native source prototype and close return', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  assert.equal(Object.getPrototypeOf(source), f.Source.prototype)
  assert.equal(source instanceof f.context.EventSource, true)
  assert.equal(f.context.EventSource.OPEN, f.Source.OPEN)
  assert.equal(source.close(), f.closeResult)
  assert.equal(source.closes, 1)
  const native = Promise.withResolvers()
  f.queue.push(native)
  const receiver = {}
  const args = ['/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal: f.signal(), method: 'POST' }]
  const original = f.context.fetch.apply(receiver, args)
  assert.equal(original, native.promise)
  assert.equal(f.requests.at(-1).receiver, receiver)
  assert.equal(f.requests.at(-1).args[1], args[1])
  native.resolve(new Response(null, { status: 204 }))
  await original
})

test('original active rejection stays identical and records live owner at reaction', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const original = f.context.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal: f.signal(), method: 'POST' })
  const cause = new TypeError('ORIGINAL_CAUSE')
  native.reject(cause)
  await assert.rejects(original, value => value === cause)
  await setImmediate()
  const event = f.snapshot().journal.find(event => event.kind === 'control-rejected-reaction')
  assert.equal(event.controlAborted, false)
  assert.equal(event.requestReasonMatches, false)
  assert.equal(event.causeClass, 'TypeError')
  assert.equal(source.closes, 0)
})

test('real native parent/composite signals preserve exact rejection reason and source close observations', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const signal = f.signal()
  const original = f.context.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal, method: 'POST' })
  const cause = new DOMException('owned native source test withdrawal', 'AbortError')
  f.control.abort(cause)
  source.close()
  native.reject(cause)
  await assert.rejects(original, error => error === cause)
  await setImmediate()
  const data = f.snapshot()
  const abort = data.journal.find(event => event.kind === 'parent-abort-event' && event.role === 'control')
  assert.equal(abort.nativeTrusted, true)
  assert.equal(abort.insideOwnAbortCall, true)
  const rejection = data.journal.find(event => event.kind === 'control-rejected-reaction')
  assert.equal(rejection.controlAborted, true)
  assert.equal(rejection.controlReasonMatches, true)
  assert.equal(rejection.requestReasonMatches, true)
  const projected = projectDirectOwners(data.journal, privateErrors, data.observerFailures, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(projected.actualGrantDocumentUnique, true)
  assert.equal(projected.actualControlOwnerUnique, true)
  assert.equal(projected.actualGenerationParentUnique, true)
  assert.equal(projected.ownerReactionStateKnown, true)
  assert.equal(projected.browserLocalOwnerOrdering.controlAbortBeforeOriginalRejectionReaction, true)
  assert.equal(projected.nativeOrderingAgainstBothPageErrors, 'UNKNOWN')
  assert.equal(projected.receiptStatus, 'PARTIAL')
  assert.equal(projected.productQualified, false)
  const output = JSON.stringify(projected)
  for (const secret of ['PRIVATE_CONNECTION_CANARY', 'PRIVATE_DOCUMENT_CANARY', 'NEVER_PUBLISH', 'localhost', 'cookie', 'Authorization'])
    assert.equal(output.includes(secret), false)
})

test('generation reason is distinct from a later control abort reason, without invented ownership', async (t) => {
  const f = fixture(t)
  await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const signal = f.signal()
  const original = f.context.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal })
  const cause = new DOMException('generation reason', 'AbortError')
  f.generation.abort(cause)
  f.control.abort(new DOMException('different control reason', 'AbortError'))
  native.reject(cause)
  await assert.rejects(original, value => value === cause)
  await setImmediate()
  const event = f.snapshot().journal.find(event => event.kind === 'control-rejected-reaction')
  assert.equal(event.requestReasonMatches, true)
  assert.equal(event.generationReasonMatches, true)
  assert.equal(event.controlReasonMatches, false)
})

test('unmapped or foreign source/connection remains unknown, never assigned to the last grant', async (t) => {
  const f = fixture(t)
  await f.authorize()
  const foreign = new f.context.EventSource('/api/installations/private/direct/DIFFERENT_CONNECTION/events')
  foreign.close()
  const data = f.snapshot()
  const out = projectDirectOwners(data.journal, [{ message: '/direct/DIFFERENT_CONNECTION/events due to access control checks.' }])
  assert.equal(out.actualGrantDocumentUnique, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
  assert.equal(out.sourceCloseObserved, false)
})

test('projector refuses malformed/ambiguous input and drops arbitrary private fields', () => {
  const unknown = projectDirectOwners(null, [{ message: '/direct/%bad/signal', Authorization: 'PRIVATE_TOKEN' }], 'PRIVATE_TOKEN')
  assert.equal(unknown.inputKnown, false)
  assert.equal(unknown.receiptStatus, 'UNKNOWN')
  assert.equal(JSON.stringify(unknown).includes('PRIVATE_TOKEN'), false)
  const many = projectDirectOwners([], [{ message: '/direct/a/signal' }, { message: '/direct/b/events' }])
  assert.equal(many.errorConnectionUnique, false)
  assert.equal(many.receiptStatus, 'UNKNOWN')
})

test('actual controller installs the same bootstrap, owns its listener and never consumes the original error', async () => {
  const page = new EventEmitter()
  let callback
  let script
  page.exposeFunction = async (_, value) => { callback = value }
  page.addInitScript = async value => { script = value }
  const errors = []
  page.on('pageerror', error => errors.push(error))
  const controller = createDirectOwnerObserver(page)
  await controller.install()
  assert.equal(script, observeDirectOwners)
  const cause = new Error('/direct/private/signal')
  page.emit('pageerror', cause)
  assert.equal(errors[0], cause)
  callback({ document: 'private', sequence: 1, kind: 'beforeunload' })
  assert.equal(controller.finish().projection.nativeOrderingAgainstBothPageErrors, 'UNKNOWN')
  controller.dispose()
  assert.equal(page.listenerCount('pageerror'), 1)
})

test('actual existing bound keeps stalled installation red; it never starts a later bootstrap', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const page = new EventEmitter()
  page.exposeFunction = () => new Promise(() => {})
  let starts = 0
  page.addInitScript = async () => { starts++ }
  const controller = createDirectOwnerObserver(page)
  const pending = controller.install()
  const rejected = assert.rejects(pending, /observation deadline/)
  await setImmediate()
  t.mock.timers.tick(7000)
  await rejected
  assert.equal(starts, 0)
  controller.dispose()
})

test('actual serialized wrappers preserve synchronous original exceptions', (t) => {
  const f = fixture(t)
  const cause = new TypeError('same original synchronous fetch refusal')
  f.queue.push({ throw: cause })
  assert.throws(() => f.context.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal: f.signal() }), error => error === cause)
})

test('missing closure or duplicate private receipt never acquires a known result', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const original = f.context.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal: f.signal() })
  const cause = new TypeError('active original')
  native.reject(cause)
  await assert.rejects(original, error => error === cause)
  await setImmediate()
  source.close()
  const data = f.snapshot()
  assert.equal(projectDirectOwners(data.journal, privateErrors).receiptStatus, 'UNKNOWN')
  const duplicate = [...data.journal, data.journal.at(-1)]
  const out = projectDirectOwners(duplicate, privateErrors, 0, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(out.relatedJournalStructurallyKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})

test('a source created before authorization is not retrospectively assigned an owner', async (t) => {
  const f = fixture(t)
  const source = new f.context.EventSource('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/events')
  await f.authorize()
  source.close()
  const out = projectDirectOwners(f.snapshot().journal, privateErrors, 0, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(out.sourceCloseProvenanceKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})

test('controller disposal failure is explicit and does not throw over a workload cause', async () => {
  const page = new EventEmitter()
  page.exposeFunction = async () => {}
  page.addInitScript = async () => {}
  const controller = createDirectOwnerObserver(page)
  await controller.install()
  page.off = () => { throw new Error('controlled observer retirement failure') }
  assert.equal(controller.dispose(), false)
  const result = controller.finish({ pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(result.projection.observerFailureCount, 1)
  assert.equal(result.projection.receiptStatus, 'UNKNOWN')
})


function actualCleanupSource() {
  const source = readFileSync(new URL('./mcps.direct-owner.overlay.ts', import.meta.url), 'utf8')
  const selected = source.slice(source.indexOf("availabilityTest('availability preserves a scoped readonly outage"))
  const start = selected.indexOf('    outage?.restore()')
  const end = selected.indexOf('\n  const directOwnerCleanupFailureCount', start)
  assert(start > 0 && end > start)
  const body = selected.slice(start, end).replace(/\n  }\n?$/, '')
  return stripTypeScriptTypes(`(async () => {${body}\n})()`)
}

test('ACTUAL overlay finally retires HTTP independently of a stalled page and keeps 7s timeout red', async (t) => {
  t.mock.timers.enable({ apis: ['setTimeout'] })
  const context = {
    outage: { restore() {} }, poll: { release() {} },
    page: { close: () => new Promise(() => {}), isClosed: () => false },
    responseGate: { close: async () => {} }, failures: [],
    directOwnerPageClosed: false, directOwnerGateClosed: false,
    setTimeout, clearTimeout, Error,
  }
  const pending = runInNewContext(actualCleanupSource(), context)
  await setImmediate()
  assert.equal(context.directOwnerGateClosed, true)
  assert.equal(context.directOwnerPageClosed, false)
  t.mock.timers.tick(7000)
  await pending
  assert.equal(context.failures.length, 1)
  assert.match(context.failures[0].message, /Outage owned cleanup deadline/)
})

test('ACTUAL overlay finally rejects a page close without an affirmative receipt', async () => {
  const context = {
    outage: { restore() {} }, poll: { release() {} },
    page: { close: async () => {}, isClosed: () => false },
    responseGate: { close: async () => {} }, failures: [],
    directOwnerPageClosed: false, directOwnerGateClosed: false,
    setTimeout, clearTimeout, Error,
  }
  await runInNewContext(actualCleanupSource(), context)
  assert.equal(context.failures.length, 1)
  assert.match(context.failures[0].message, /closure UNKNOWN/)
  assert.equal(context.directOwnerGateClosed, true)
})

test('ACTUAL overlay finally preserves an original closure error and still closes the other owner', async () => {
  const cause = new Error('same original HTTP close failure')
  const context = {
    outage: { restore() {} }, poll: { release() {} },
    page: { close: async () => {}, isClosed: () => true },
    responseGate: { close: async () => { throw cause } }, failures: [],
    directOwnerPageClosed: false, directOwnerGateClosed: false,
    setTimeout, clearTimeout, Error,
  }
  await runInNewContext(actualCleanupSource(), context)
  assert.equal(context.failures[0], cause)
  assert.equal(context.directOwnerPageClosed, true)
  assert.equal(context.directOwnerGateClosed, false)
})


function sourceFiles(t) {
  const targets = ['tests/e2e/mcps.spec.ts', 'apps/web/tests/direct-transport.test.ts']
  const originals = Object.fromEntries(targets.map(target => [target, execFileSync('git', ['-c', `safe.directory=${process.cwd()}`, 'show', `${product}:${target}`])]))
  mkdirSync('test-results/source-only-direct-owner', { recursive: true })
  const root = mkdtempSync(path.resolve('test-results/source-only-direct-owner/fixture-'))
  const evidence = path.join(root, 'private-evidence')
  for (const target of targets) {
    mkdirSync(path.dirname(path.join(root, target)), { recursive: true })
    writeFileSync(path.join(root, target), originals[target])
  }
  t.after(() => rmSync(root, { recursive: true, force: true }))
  return { root, evidence, targets, originals }
}

test('actual fixed overlay controller applies only the two pinned tests and restores exact originals', (t) => {
  const f = sourceFiles(t)
  const applied = applyTestOverlays(f.root)
  assert.equal(applied.changedTests, 2)
  assert.equal(applied.productQualified, false)
  const result = restoreTestOverlays(f.root, f.evidence, f.originals)
  assert.equal(result.originalRestored, true)
  assert.equal(result.knownBefore, true)
  for (const target of f.targets)
    assert.deepEqual(readFileSync(path.join(f.root, target)), f.originals[target])
})

test('actual fixed controller refuses wrong source before changing either file', (t) => {
  const f = sourceFiles(t)
  writeFileSync(path.join(f.root, f.targets[1]), 'UNEXPECTED_SOURCE')
  assert.throws(() => applyTestOverlays(f.root), /exact pinned test/)
  assert.deepEqual(readFileSync(path.join(f.root, f.targets[0])), f.originals[f.targets[0]])
})

test('actual restoration preserves unexpected bytes privately and cannot claim known identity', (t) => {
  const f = sourceFiles(t)
  applyTestOverlays(f.root)
  writeFileSync(path.join(f.root, f.targets[0]), 'UNEXPECTED_PRIVATE_BYTES')
  const result = restoreTestOverlays(f.root, f.evidence, f.originals)
  assert.equal(result.knownBefore, false)
  assert.equal(result.productQualified, false)
  assert.equal(readFileSync(path.join(f.evidence, 'unexpected-test-0.private'), 'utf8'), 'UNEXPECTED_PRIVATE_BYTES')
  for (const target of f.targets)
    assert.deepEqual(readFileSync(path.join(f.root, target)), f.originals[target])
})

test('unit restoration never silently restores or qualifies the still-overlaid native case', (t) => {
  const f = sourceFiles(t)
  applyTestOverlays(f.root)
  const native = readFileSync(path.join(f.root, f.targets[0]))
  const result = restoreTestOverlays(f.root, f.evidence, f.originals, true)
  assert.equal(result.unitOnly, true)
  assert.deepEqual(readFileSync(path.join(f.root, f.targets[1])), f.originals[f.targets[1]])
  assert.deepEqual(readFileSync(path.join(f.root, f.targets[0])), native)
  assert.equal(result.productQualified, false)
})

test('serialized lifecycle provenance checks the actual captured document, not a claimed document label', async (t) => {
  const f = fixture(t)
  await f.authorize()
  f.context.lifecycleCallback = f.callbacks.get('beforeunload')
  runInContext(`{
    const event = new BeforeUnloadEvent('beforeunload');
    Object.defineProperties(event, {isTrusted:{value:true},target:{value:document},currentTarget:{value:window}});
    lifecycleCallback(event);
  }`, f.context)
  let event = f.snapshot().journal.at(-1)
  assert.equal(event.kind, 'beforeunload')
  assert.equal(event.nativeTrusted, true)
  assert.equal(event.ownContext, true)
  runInContext(`{
    const event = new BeforeUnloadEvent('beforeunload');
    Object.defineProperties(event, {isTrusted:{value:true},target:{value:{}},currentTarget:{value:window}});
    lifecycleCallback(event);
  }`, f.context)
  event = f.snapshot().journal.at(-1)
  assert.equal(event.nativeTrusted, false)
  runInContext(`document={defaultView:window};`, f.context)
  f.context.lifecycleCallback(new Event('beforeunload'))
  event = f.snapshot().journal.at(-1)
  assert.equal(event.ownContext, false)
  assert.equal(event.nativeTrusted, false)
})
