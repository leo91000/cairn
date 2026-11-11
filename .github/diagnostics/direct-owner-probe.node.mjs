import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { execFileSync, spawnSync } from 'node:child_process'
import path from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { stripTypeScriptTypes } from 'node:module'
import { test } from 'node:test'
import { setImmediate } from 'node:timers/promises'
import { createContext, runInContext, runInNewContext } from 'node:vm'
import { applyTestOverlays, restoreTestOverlays, product, verifySourceTrustedChain, selectedSourceGateScript, sourceGateCommands, historicalWorkflowReference, historicalSourceWorkflow, verifyHistoricalSourceAcquisition } from './direct-owner-overlay-control.mjs'
import { observeDirectOwners, projectDirectOwners, createDirectOwnerObserver } from './direct-owner-probe.mjs'

function recorderBootstrapSource() {
  const reference = process.env.C40_RECORDER_REFERENCE
  if (!reference)
    return observeDirectOwners.toString()
  assert.match(reference, /^[a-f0-9]{40}$/)
  const source = execFileSync('git', ['show', `${reference}:.github/diagnostics/direct-owner-probe.mjs`], { encoding: 'utf8' })
  const start = source.indexOf('export function observeDirectOwners(')
  const end = source.indexOf('\nfunction errorIdentity(', start)
  assert(start >= 0 && end > start)
  return source.slice(start, end).replace('export function', 'function')
}

function fixture(t, options = {}) {
  const originalAny = AbortSignal.any
  const anyInputs = []
  const anyResults = []
  if (options.sequenceAny) {
    AbortSignal.any = function (input) {
      // Source stand-in for browser WebIDL sequence conversion. Node's Array
      // API is not credited as a native browser iterable-conversion result.
      const values = Array.from(input)
      anyInputs.push(values)
      const result = Reflect.apply(originalAny, this, [values])
      anyResults.push(result)
      return result
    }
  }
  const originalAbort = AbortController.prototype.abort
  if (options.abortFailure)
    AbortController.prototype.abort = function () { throw options.abortFailure }
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
  const faults = { markerWrite: false, journalWrite: false }
  const failureSignals = []
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
    close() { this.readyState = 2; this.closes++; if (options.closeFailure) throw options.closeFailure; return closeResult }
  }
  const nativeFetch = function (...args) {
    requests.push({ receiver: this, args })
    const next = queue.shift()
    if (!next)
      throw new Error('controlled source fixture has no queued original')
    if (typeof next === 'function')
      return Reflect.apply(next, this, args)
    if (next.throw)
      throw next.throw
    return next.promise
  }
  const context = createContext({
    AbortController, AbortSignal, Response, Event, EventSource: Source, Error, Object, Array,
    DOMException, URL, Map, WeakMap, Proxy, Promise, performance,
    BeforeUnloadEvent: class extends Event {}, PageTransitionEvent: class extends Event {},
    sessionStorage: { getItem: key => storage.get(key) ?? null, setItem: (key, value) => {
      if (faults.markerWrite && key === '__c40_direct_owner_journal_v1:observer-failure' && value === '1') throw new Error('controlled marker persistence failure')
      if (faults.journalWrite && key === '__c40_direct_owner_journal_v1') throw new Error('controlled journal persistence failure')
      return storage.set(key, value)
    } },
    console: { error(...args) { failureSignals.push(args); options.onFailureSignal?.(...args) } },
    fetch: nativeFetch,
    __c40RecordDirectOwner: event => { ingresses.push(event); return Promise.resolve() },
    addEventListener: (name, callback) => { callbacks.set(name, callback) },
  })
  runInContext("window=globalThis; top=window; document={defaultView:window}; location={href:'http://localhost:4422/installations/private/agents',origin:'http://localhost:4422',pathname:'/installations/private/agents'}; cairnAvailabilityDocument='PRIVATE_DOCUMENT_CANARY'", context)
  const installationScope = { installationId: options.installationId ?? 'private', installationSlug: options.installationSlug ?? 'private' }
  if (options.installationSlug)
    runInContext(`location.pathname=${JSON.stringify('/installations/' + options.installationSlug + '/agents')};location.href=location.origin+location.pathname`, context)
  runInContext(`(${recorderBootstrapSource()})(${JSON.stringify(installationScope)})`, context)
  const ownWindow = runInContext('window', context)
  const invokeFetch = (...args) => Reflect.apply(context.fetch, ownWindow, args)
  const generation = new AbortController()
  const control = new AbortController()
  const signal = () => context.AbortSignal.any([generation.signal, control.signal])
  return {
    context, fetch: invokeFetch, nativeFetch, Source, generation, control, signal, queue, requests, storage, ingresses, installationScope,
    closeResult, originalJsonPromises, callbacks, faults, failureSignals, unwrappedAbort: originalAbort, anyInputs, anyResults,
    snapshot: () => context.__c40DirectOwnerReceipt.snapshot(),
    async authorize(sourceInstallation = installationScope.installationId) {
      const native = Promise.withResolvers()
      queue.push(native)
      const options = { method: 'POST', signal: signal() }
      const returned = invokeFetch(`/api/installations/${installationScope.installationId}/direct/authorize`, options)
      assert.equal(returned, native.promise)
      native.resolve(Response.json({ available: true, grant: { claims: { connection_id: 'PRIVATE_CONNECTION_CANARY', token: 'NEVER_PUBLISH' } } }))
      const response = await returned
      const body = response.json()
      assert.equal(body, originalJsonPromises.get(response))
      await body
      await setImmediate()
      return new context.EventSource(`/api/installations/${sourceInstallation}/direct/PRIVATE_CONNECTION_CANARY/events`)
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
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal: f.signal(), method: 'POST' })
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
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { signal, method: 'POST' })
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
  assert.equal(projected.lifecycleObserved, false)
  assert.equal(projected.receiptStatus, 'UNKNOWN')
  assert.equal(projected.productQualified, false)
  assert.equal(projected.ownershipEvidenceScope, 'PINNED_SOURCE_CONDITIONAL')
  assert.equal(projected.nativeOperationEvidenceScope, 'PINNED_SOURCE_CONDITIONAL')
  assert.equal(projected.universalNativeOperationEquivalence, false)
  assert.equal(projected.outOfDomainOperationEquivalence, 'NOT_CLAIMED')
  assert.equal(projected.universalRuntimeAdmission, false)
  assert.equal(projected.runtimeProducerReceipt, false)
  assert.equal(projected.proxyDetection, 'NOT_PROVIDED')
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
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal })
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
  let bootstrapScope
  page.exposeFunction = async (_, value) => { callback = value }
  page.addInitScript = async (value, scope) => { script = value; bootstrapScope = scope }
  const errors = []
  page.on('pageerror', error => errors.push(error))
  const controller = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
  await controller.install()
  assert.equal(script, observeDirectOwners)
  assert.deepEqual(bootstrapScope, { installationId: 'private', installationSlug: 'private' })
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
  const controller = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
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
  assert.throws(() => f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: f.signal() }), error => error === cause)
})

test('missing closure or duplicate private receipt never acquires a known result', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: f.signal() })
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
  const controller = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
  await controller.install()
  const off = page.off.bind(page)
  page.off = (kind, listener) => {
    if (kind === 'pageerror')
      throw new Error('controlled observer retirement failure')
    return off(kind, listener)
  }
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


// Fixture acquisition trusts only its explicitly controlled repository, per Git child.
// No host/global configuration is written; all original Git status/stderr survives.
function controlledSourceGitArgs(args, root = process.cwd()) {
  return ['-c', `safe.directory=${path.resolve(root)}`, ...args]
}

function controlledSourceGitEnvironment(env, root = process.cwd()) {
  const count = Number(env.GIT_CONFIG_COUNT ?? 0)
  assert(Number.isSafeInteger(count) && count >= 0)
  return {
    ...env,
    GIT_CONFIG_COUNT: String(count + 1),
    [`GIT_CONFIG_KEY_${count}`]: 'safe.directory',
    [`GIT_CONFIG_VALUE_${count}`]: path.resolve(root),
  }
}

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

test('H75 all sources on the errored connection, including an unmatched source, prevent known provenance', async (t) => {
  const f = fixture(t)
  const unmatched = new f.context.EventSource('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/events')
  const matched = await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: f.signal() })
  const cause = new TypeError('same original')
  native.reject(cause)
  await assert.rejects(original, value => value === cause)
  await setImmediate()
  unmatched.close()
  matched.close()
  const out = projectDirectOwners(f.snapshot().journal, privateErrors, 0, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(out.actualSourceUnique, false)
  assert.equal(out.sourceCloseProvenanceKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})

test('H75 a rejection from another actual parent owner cannot stand for the selected grant owner', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  const other = new AbortController()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: f.context.AbortSignal.any([f.generation.signal, other.signal]) })
  const cause = new TypeError('different actual owner')
  native.reject(cause)
  await assert.rejects(original, value => value === cause)
  await setImmediate()
  source.close()
  const out = projectDirectOwners(f.snapshot().journal, privateErrors, 0, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(out.ownerReactionStateKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})

test('H75 an orphan rejection request identity remains unknown even with a matched source', async (t) => {
  const f = fixture(t)
  const source = await f.authorize()
  const native = Promise.withResolvers()
  f.queue.push(native)
  const original = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: f.signal() })
  const cause = new TypeError('original orphan metadata control')
  native.reject(cause)
  await assert.rejects(original, value => value === cause)
  await setImmediate()
  source.close()
  const journal = Array.from(f.snapshot().journal, event => event.kind === 'control-rejected-reaction' ? { ...event, request: event.request + 100 } : event)
  const out = projectDirectOwners(journal, privateErrors, 0, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(out.ownerReactionStateKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})


test('close provenance does not turn an original thrown close into a known return', async (t) => {
  const cause = new Error('same original close failure')
  const f = fixture(t, { closeFailure: cause })
  const source = await f.authorize()
  assert.throws(() => source.close(), value => value === cause)
  const journal = f.snapshot().journal
  assert.equal(journal.some(event => event.kind === 'source-close-threw'), true)
  assert.equal(journal.some(event => event.kind === 'source-close-return'), false)
  const out = projectDirectOwners(journal, privateErrors, 0, { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 })
  assert.equal(out.sourceCloseProvenanceKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})


async function completeSourceReceipt(t, options = {}) {
  const f = fixture(t, options)
  const source = await f.authorize(options.sourceInstallation ?? f.installationScope.installationId)
  const native = Promise.withResolvers()
  f.queue.push(native)
  const signal = options.alternateGeneration ? f.context.AbortSignal.any([new AbortController().signal, f.control.signal]) : f.signal()
  const original = f.fetch(`/api/installations/${options.requestInstallation ?? f.installationScope.installationId}/direct/PRIVATE_CONNECTION_CANARY/signal`, { method: options.method ?? 'POST', signal })
  const cause = new TypeError('same original controlled source terminal')
  native.reject(cause)
  await assert.rejects(original, value => value === cause)
  await setImmediate()
  source.close()
  // Controlled SOURCE events exercise the actual serialized listener, not a
  // claim that Node acquired native browser lifecycle receipts.
  for (const kind of ['beforeunload', 'pagehide']) {
    f.context.lifecycleCallback = f.callbacks.get(kind)
    runInContext(`{
      const event = new ${kind === 'beforeunload' ? 'BeforeUnloadEvent' : 'PageTransitionEvent'}('${kind}');
      Object.defineProperties(event, {isTrusted:{value:true},target:{value:document},currentTarget:{value:window}});
      lifecycleCallback(event);
    }`, f.context)
  }
  f.originalCause = cause
  return options.returnFixture ? f : f.snapshot().journal
}
const completeClosure = { pageClosed: true, gateClosed: true, cleanupFailureCount: 0 }

test('H76 missing or ambiguous native lifecycle receipts must stay UNKNOWN', async (t) => {
  const complete = await completeSourceReceipt(t)
  assert.equal(projectDirectOwners(complete, privateErrors, 0, completeClosure).receiptStatus, 'SOURCE_CONDITIONAL_PARTIAL')
  for (const kind of ['beforeunload', 'pagehide']) {
    const missing = complete.filter(event => event.kind !== kind)
    assert.equal(projectDirectOwners(missing, privateErrors, 0, completeClosure).receiptStatus, 'UNKNOWN')
    const duplicate = complete.map(event => event.kind === kind ? { ...event, sequence: 1000 } : event)
    duplicate.push(complete.find(event => event.kind === kind))
    assert.equal(projectDirectOwners(duplicate, privateErrors, 0, completeClosure).receiptStatus, 'UNKNOWN')
  }
})

test('H76 actual binding and snapshot retain conflicting receipts and refuse provenance', async (t) => {
  const complete = await completeSourceReceipt(t)
  const page = new EventEmitter()
  let binding
  page.exposeFunction = async (_, callback) => { binding = callback }
  page.addInitScript = async () => {}
  const controller = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
  await controller.install()
  t.after(() => controller.dispose())
  for (const error of privateErrors)
    page.emit('pageerror', error)
  for (const event of complete)
    binding(event)
  controller.accept({ journal: complete.map(event => ({ ...event })), observerFailures: 0 })
  assert.equal(controller.finish(completeClosure).projection.receiptStatus, 'SOURCE_CONDITIONAL_PARTIAL')
  const target = complete.find(event => event.kind === 'source-close-return')
  const conflict = { ...target, readyState: 0 }
  controller.accept({ journal: [conflict], observerFailures: 0 })
  binding({ ...target, readyState: 1 })
  const result = controller.finish(completeClosure)
  assert.equal(result.projection.receiptStatus, 'UNKNOWN')
  assert.equal(result.projection.observerFailureCount, 2)
  assert.equal(result.privateEvidence.conflicts.length, 2)
  assert.equal(result.privateEvidence.journal.find(event => event.sequence === target.sequence).readyState, 2)
  assert.deepEqual(result.privateEvidence.conflicts.map(value => value.event.readyState), [0, 1])
})


test('H76 lifecycle receipt refuses gaps, wrong context, untrusted or reversed lifecycle', async (t) => {
  const complete = await completeSourceReceipt(t)
  const variants = [
    complete.filter(event => event.kind !== 'control-headers'),
    complete.map(event => event.kind === 'pagehide' ? { ...event, ownContext: false } : event),
    complete.map(event => event.kind === 'pagehide' ? { ...event, nativeTrusted: false } : event),
    complete.map(event => event.kind === 'pagehide' ? { ...event, sequence: complete.find(value => value.kind === 'beforeunload').sequence } : event),
  ]
  for (const journal of variants)
    assert.equal(projectDirectOwners(journal, privateErrors, 0, completeClosure).receiptStatus, 'UNKNOWN')
})


for (const throwsOnRead of [1, 2]) {
  test(`H78 selected recorder preserves original abort with an own aborted getter throwing on read ${throwsOnRead}`, async (t) => {
    const f = fixture(t)
    await f.authorize()
    const signal = f.control.signal
    let reads = 0
    Object.defineProperty(signal, 'aborted', { get() {
      if (++reads === throwsOnRead)
        throw new Error('observer property getter refusal')
      return false
    } })
    const cause = new DOMException('same original reason', 'AbortError')
    assert.doesNotThrow(() => f.control.abort(cause))
    const aborted = Object.getOwnPropertyDescriptor(AbortSignal.prototype, 'aborted').get
    assert.equal(Reflect.apply(aborted, signal, []), true)
    assert.equal(reads, 0)
    const journal = f.snapshot().journal
    assert.equal(journal.filter(event => event.kind === 'parent-abort-call-enter' && event.role === 'control').length, 1)
    assert.equal(journal.filter(event => event.kind === 'parent-abort-call-return' && event.role === 'control').length, 1)
    assert.equal(f.snapshot().observerFailures, 0)
  })
}

test('H78 selected recorder keeps an ORIGINAL abort exception and still retires its observation bracket', async (t) => {
  const cause = new Error('original controlled abort throw')
  const f = fixture(t, { abortFailure: cause })
  await f.authorize()
  Object.defineProperty(f.control.signal, 'aborted', { get() { throw new Error('metadata must not replace original cause') } })
  assert.throws(() => f.control.abort(), error => error === cause)
  // A later actual native abort outside the wrapper must not inherit a leaked
  // recorder bracket from that throwing invocation.
  Reflect.apply(f.unwrappedAbort, f.control, [new DOMException('later outside-call reason', 'AbortError')])
  const journal = f.snapshot().journal
  assert.equal(journal.find(event => event.kind === 'parent-abort-event' && event.role === 'control').insideOwnAbortCall, false)
  assert.equal(journal.filter(event => event.kind === 'parent-abort-call-enter' && event.role === 'control').length, 1)
  assert.equal(journal.filter(event => event.kind === 'parent-abort-call-return' && event.role === 'control').length, 1)
  assert.equal(f.snapshot().observerFailures, 0)
})


for (const inherited of [false, true]) {
  test(`H79 selected fetch refuses signal provenance invalidated by ${inherited ? 'inherited' : 'own'} conversion`, async (t) => {
    const f = fixture(t)
    await f.authorize()
    const actualControl = new AbortController()
    const usedSignal = f.context.AbortSignal.any([f.generation.signal, actualControl.signal])
    const nominal = f.signal()
    const options = { signal: nominal }
    const original = Promise.resolve(new Response(null, { status: 204 }))
    let used
    f.queue.push((_, init) => { void init.method; used = init.signal; return original })
    const target = inherited ? Object.prototype : options
    const prior = Object.getOwnPropertyDescriptor(target, 'method')
    Object.defineProperty(target, 'method', { configurable: true, get() {
      if (this === options)
        options.signal = usedSignal
      return 'POST'
    } })
    let returned
    try { returned = f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', options) }
    finally {
      if (prior)
        Object.defineProperty(target, 'method', prior)
      else
        Reflect.deleteProperty(target, 'method')
    }
    assert.equal(returned, original)
    assert.equal(used, usedSignal)
    await returned
    await setImmediate()
    const snapshot = f.snapshot()
    assert.equal(snapshot.journal.some(event => event.kind === 'control-start' && event.category === 'signal' && event.parentIdentityKnown), false)
    assert.equal(snapshot.observerFailures > 0, true)
  })
}

test('H79 selected fetch still observes the real caller shape with plain string headers and JSON body', async (t) => {
  const f = fixture(t)
  await f.authorize()
  const original = Promise.resolve(new Response(null, { status: 204 }))
  f.queue.push({ promise: original })
  const options = { method: 'POST', signal: f.signal(), headers: { 'Content-Type': 'application/json', 'X-Source-Test': 'ordinary' }, body: '{}' }
  assert.equal(f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', options), original)
  await original
  await setImmediate()
  assert.equal(f.snapshot().journal.some(event => event.kind === 'control-start' && event.category === 'signal' && event.parentIdentityKnown), true)
  assert.equal(f.snapshot().observerFailures, 0)
})

for (const selfRemoving of [false, true]) {
  test(`H79 any sequence rejects custom iteration without changing the original composite (self-removing ${selfRemoving})`, async (t) => {
    const f = fixture(t, { sequenceAny: true })
    const actualGeneration = new AbortController()
    const actualControl = new AbortController()
    const nominal = [f.generation.signal, f.control.signal]
    let iterations = 0
    Object.defineProperty(nominal, Symbol.iterator, { configurable: true, value: function* () {
      iterations++
      yield actualGeneration.signal
      yield actualControl.signal
      if (selfRemoving)
        Reflect.deleteProperty(nominal, Symbol.iterator)
    } })
    const composite = f.context.AbortSignal.any(nominal)
    assert.equal(composite, f.anyResults.at(-1))
    assert.equal(iterations, 1)
    assert.deepEqual(f.anyInputs.at(-1), [actualGeneration.signal, actualControl.signal])
    const original = Promise.resolve(new Response(null, { status: 204 }))
    f.queue.push({ promise: original })
    assert.equal(f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: composite }), original)
    await original
    await setImmediate()
    assert.equal(f.snapshot().journal.some(event => event.kind === 'control-start' && event.parentIdentityKnown), false)
  })
}

test('H79 any sequence never consumes index accessors for observation', async (t) => {
  const f = fixture(t, { sequenceAny: true })
  let reads = 0
  const nominal = [f.generation.signal, f.control.signal]
  Object.defineProperty(nominal, '0', { get() { reads++; return f.generation.signal } })
  const composite = f.context.AbortSignal.any(nominal)
  assert.equal(composite, f.anyResults.at(-1))
  assert.equal(reads, 1)
  const original = Promise.resolve(new Response(null, { status: 204 }))
  f.queue.push({ promise: original })
  assert.equal(f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', { method: 'POST', signal: composite }), original)
  await original
  await setImmediate()
  assert.equal(f.snapshot().journal.some(event => event.kind === 'control-start' && event.parentIdentityKnown), false)
})


test('H80 original POST receipt refuses a different actual generation parent on the same controller', async (t) => {
  const journal = await completeSourceReceipt(t, { alternateGeneration: true })
  const out = projectDirectOwners(journal, privateErrors, 0, completeClosure)
  assert.equal(out.ownerReactionStateKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})

test('H80 same connection string cannot borrow a grant from another installation', async (t) => {
  const journal = await completeSourceReceipt(t, { sourceInstallation: 'FOREIGN_INSTALLATION', requestInstallation: 'FOREIGN_INSTALLATION' })
  const errors = privateErrors.map(value => ({ message: value.message.replace('/installations/private/', '/installations/FOREIGN_INSTALLATION/') }))
  assert.equal(journal.find(event => event.kind === 'source-created').grantMatched, false)
  assert.equal(projectDirectOwners(journal, errors, 0, completeClosure).receiptStatus, 'UNKNOWN')
})

test('H80 GET cannot satisfy the requested original signalling POST receipt', async (t) => {
  const journal = await completeSourceReceipt(t, { method: 'GET' })
  const out = projectDirectOwners(journal, privateErrors, 0, completeClosure)
  assert.equal(out.ownerReactionStateKnown, false)
  assert.equal(out.receiptStatus, 'UNKNOWN')
})


function actualChainManifest() {
  return JSON.parse(readFileSync(new URL('./overlay-manifest.json', import.meta.url), 'utf8'))
}

test('SOURCE-TRUSTED actual chain validator binds the real pinned producer and every selected delegate', () => {
  const result = verifySourceTrustedChain(process.cwd())
  assert.equal(result.mode, 'PINNED_SOURCE_CONDITIONAL')
  assert.equal(result.sourceChainBytesMatched, true)
  assert.equal(result.runtimeProducerReceipt, false)
  assert.equal(result.universalRuntimeAdmission, false)
  assert.equal(result.runtimeProxyDetection, false)
  assert.equal(result.nativeQualified, false)
})

test('SOURCE-TRUSTED actual chain validator refuses a changed producer hash without touching any source', () => {
  const manifest = actualChainManifest()
  manifest.sourceTrusted.productFiles['apps/web/src/availability-read.ts'].sha256 = '0'.repeat(64)
  assert.throws(() => verifySourceTrustedChain(process.cwd(), path.dirname(fileURLToPath(import.meta.url)), manifest), /producer-chain source hash mismatch/)
})

test('SOURCE-TRUSTED actual chain validator refuses changed delegate bytes in its manifest', () => {
  const manifest = actualChainManifest()
  manifest.sourceTrusted.observerFiles['direct-owner-probe.mjs'].sha256 = '0'.repeat(64)
  assert.throws(() => verifySourceTrustedChain(process.cwd(), path.dirname(fileURLToPath(import.meta.url)), manifest), /observer-chain source hash mismatch/)
})

test('SOURCE-TRUSTED actual chain validator refuses removal of a required delegate', () => {
  const manifest = actualChainManifest()
  delete manifest.sourceTrusted.observerFiles['mcps.direct-owner.overlay.ts']
  assert.throws(() => verifySourceTrustedChain(process.cwd(), path.dirname(fileURLToPath(import.meta.url)), manifest))
})

test('SOURCE-TRUSTED declaration never admits a universal or runtime producer receipt', () => {
  for (const key of ['universalRuntimeAdmission', 'runtimeProducerReceipt', 'runtimeProxyDetection']) {
    const manifest = actualChainManifest()
    manifest.sourceTrusted[key] = true
    assert.throws(() => verifySourceTrustedChain(process.cwd(), path.dirname(fileURLToPath(import.meta.url)), manifest))
  }
})


test('H81 actual route slug and Direct API UUID are coherently bound without treating them as equal', async (t) => {
  const id = '11111111-1111-4111-8111-111111111111'
  const journal = await completeSourceReceipt(t, { installationId: id, installationSlug: 'source-installation-slug' })
  const errors = privateErrors.map(value => ({ message: value.message.replace('/installations/private/', `/installations/${id}/`) }))
  const out = projectDirectOwners(journal, errors, 0, completeClosure)
  assert.equal(out.receiptStatus, 'SOURCE_CONDITIONAL_PARTIAL')
  assert.equal(out.ownerReactionStateKnown, true)
  assert.equal(out.sourceCloseProvenanceKnown, true)
  assert.equal(JSON.stringify(out).includes(id), false)
  assert.equal(JSON.stringify(out).includes('source-installation-slug'), false)
})


test('H81 missing source fixture mapping is a diagnostic setup failure before any native action', () => {
  const page = new EventEmitter()
  assert.throws(() => createDirectOwnerObserver(page), /installation mapping unavailable/)
  assert.throws(() => createDirectOwnerObserver(page, { installationId: '', installationSlug: 'route' }), /installation mapping unavailable/)
  assert.equal(page.listenerCount('pageerror'), 0)
})

test('H81 a foreign API UUID cannot borrow a valid slug mapping', async (t) => {
  const id = '11111111-1111-4111-8111-111111111111'
  const journal = await completeSourceReceipt(t, { installationId: id, installationSlug: 'source-installation-slug', sourceInstallation: 'FOREIGN_INSTALLATION', requestInstallation: 'FOREIGN_INSTALLATION' })
  const errors = privateErrors.map(value => ({ message: value.message.replace('/installations/private/', '/installations/FOREIGN_INSTALLATION/') }))
  assert.equal(projectDirectOwners(journal, errors, 0, completeClosure).receiptStatus, 'UNKNOWN')
})


test('H82 selected workflow script explicitly enables product and two limit-recognition controls', () => {
  const workflow = readFileSync(new URL('./selected-workflow.yaml', import.meta.url), 'utf8')
  assert.equal(selectedSourceGateScript(workflow), sourceGateCommands.join('\n') + '\n')
})

test('H82 actual source gate validator rejects the immutable old combined runner', () => {
  const workflow = historicalSourceWorkflow()
  assert.throws(() => selectedSourceGateScript(workflow), /complete shell block/)
})

test('H82 source gate cannot omit the recognition selector while claiming that contract', () => {
  const workflow = readFileSync(new URL('./selected-workflow.yaml', import.meta.url), 'utf8').replace(' --source-trusted-limit', '')
  assert.throws(() => selectedSourceGateScript(workflow), /limit recognition explicitly/)
})


function finalIdentityFixture(t, mode) {
  const f = sourceFiles(t)
  mkdirSync(f.evidence, { recursive: true })
  const workflow = readFileSync(new URL('./selected-workflow.yaml', import.meta.url), 'utf8')
  const name = 'ALWAYS restore exact original tests and product identity'
  const blocks = workflow.split('      - name: ').filter(value => value.startsWith(name + '\n'))
  assert.equal(blocks.length, 1)
  const lines = blocks[0].split('\n')
  const first = lines.indexOf('        run: |') + 1
  assert(first > 0)
  const body = []
  for (const line of lines.slice(first)) {
    if (!line.startsWith('          '))
      break
    body.push(line.slice(10))
  }
  let script = body.join('\n') + '\n'
  const restore = 'node test-results/direct-owner-receipt/direct-owner-overlay-control.mjs restore'
  assert(script.includes(restore))
  // Controlled shell failure at the existing restore seam; no native claim and
  // no copied finalization logic. Actual restoration itself is tested above.
  script = script.replace(restore, mode === 'restore-failure' || mode === 'identity-unknown' ? 'exit 37' : ':')
  const proof = path.join(f.evidence, 'observer-source.proof')
  writeFileSync(proof, 'SOURCE FIXTURE')
  const digest = mode === 'hash-failure' ? '0'.repeat(64) : execFileSync('sha256sum', [proof], { encoding: 'utf8' }).split(' ')[0]
  writeFileSync(path.join(f.evidence, 'observer-source.sha256'), digest + '  ' + proof + '\n')
  const env = { ...process.env, C40_EVIDENCE_DIR: f.evidence, GITHUB_WORKSPACE: process.cwd(), PRODUCT_SHA: execFileSync('git', controlledSourceGitArgs(['rev-parse', 'HEAD']), { encoding: 'utf8' }).trim() }
  if (mode === 'identity-unknown')
    env.GIT_DIR = path.join(f.root, 'missing-owned-git-directory')
  const result = spawnSync('bash', ['-e', '-o', 'pipefail', '-c', script], { cwd: process.cwd(), env, encoding: 'utf8' })
  return { ...f, result, identity: path.join(f.evidence, 'final-identity.txt') }
}

test('H84 selected workflow explicitly acquires its bound historical source before binding', () => {
  const workflow = readFileSync(new URL('./selected-workflow.yaml', import.meta.url), 'utf8')
  verifyHistoricalSourceAcquisition(workflow)
  assert.equal(historicalWorkflowReference.commit, '7aac8c145f1e44d764c9f7150b01727fd0a1f19d')
  assert.throws(() => selectedSourceGateScript(historicalSourceWorkflow()), /complete shell block/)
})

test('H84 missing historical acquisition is refused even in a checkout that happens to hold the object', () => {
  const workflow = readFileSync(new URL('./selected-workflow.yaml', import.meta.url), 'utf8').replace(' "$OBSERVER_SHA" ' + historicalWorkflowReference.commit, ' "$OBSERVER_SHA"')
  assert.throws(() => verifyHistoricalSourceAcquisition(workflow), /explicitly acquired/)
})

test('H84 actual Git cannot read the historical source from an empty object view', (t) => {
  const f = sourceFiles(t)
  const objects = path.join(f.root, 'empty-owned-objects')
  mkdirSync(objects)
  const result = spawnSync('git', controlledSourceGitArgs(['show', `${historicalWorkflowReference.commit}:${historicalWorkflowReference.path}`]), {
    env: { ...process.env, GIT_OBJECT_DIRECTORY: objects, GIT_ALTERNATE_OBJECT_DIRECTORIES: '', GIT_NO_LAZY_FETCH: '1' }, encoding: 'utf8',
  })
  assert.notEqual(result.status, 0)
  assert.equal(result.status, 128)
  assert.equal(result.stdout, '')
  assert.doesNotMatch(String(result.stderr), /dubious ownership/)
})

for (const mode of ['restore-failure', 'hash-failure', 'identity-unknown', 'success']) {
  test(`H84 actual final workflow shell captures identity and preserves ${mode}`, (t) => {
    const f = finalIdentityFixture(t, mode)
    assert.equal(f.result.status, mode === 'restore-failure' || mode === 'identity-unknown' ? 37 : mode === 'hash-failure' ? 1 : 0)
    assert.equal(existsSync(f.identity), true)
    const identity = readFileSync(f.identity, 'utf8')
    if (mode === 'identity-unknown') {
      assert.equal(identity, 'product=UNKNOWN tree=UNKNOWN identityCaptured=false\n')
    }
    else {
      const actual = execFileSync('git', controlledSourceGitArgs(['log', '-1', '--format=product=%H tree=%T']), { encoding: 'utf8' })
      assert.equal(identity, actual)
    }
  })
}


async function restorationSource(f) {
  const reference = process.env.C40_RESTORE_REFERENCE
  if (!reference)
    return { applyTestOverlays, restoreTestOverlays, product, sourceFile: fileURLToPath(new URL('./direct-owner-overlay-control.mjs', import.meta.url)) }
  assert.match(reference, /^[a-f0-9]{40}$/)
  const directory = path.join(f.root, 'owned-reference-control')
  mkdirSync(directory)
  for (const name of ['direct-owner-overlay-control.mjs', 'overlay-manifest.json', 'mcps.direct-owner.overlay.ts', 'direct-transport.seam.overlay.ts']) {
    const bytes = execFileSync('git', ['show', `${reference}:.github/diagnostics/${name}`])
    writeFileSync(path.join(directory, name), bytes)
  }
  const sourceFile = path.join(directory, 'direct-owner-overlay-control.mjs')
  const controller = await import(pathToFileURL(sourceFile).href)
  for (const target of f.targets) {
    f.originals[target] = execFileSync('git', ['show', `${controller.product}:${target}`])
    writeFileSync(path.join(f.root, target), f.originals[target])
  }
  return { ...controller, sourceFile }
}

for (const mode of ['first-read-failure', 'first-preservation-failure', 'both-read-failures', 'first-original-hash-failure']) {
  test(`H86 actual overlay controller attempts every target independently after ${mode}`, async (t) => {
    const f = sourceFiles(t)
    const controller = await restorationSource(f)
    controller.applyTestOverlays(f.root)
    const beforeFirst = readFileSync(path.join(f.root, f.targets[0]))
    const originals = mode === 'first-original-hash-failure' ? { ...f.originals, [f.targets[0]]: Buffer.from('CORRUPT SOURCE ORIGINAL') } : f.originals
    if (mode === 'first-preservation-failure') {
      writeFileSync(path.join(f.root, f.targets[0]), 'UNKNOWN SOURCE BYTES MUST REMAIN')
      mkdirSync(path.join(f.evidence, 'unexpected-test-0.private'), { recursive: true })
    }
    else if (mode !== 'first-original-hash-failure') {
      rmSync(path.join(f.root, f.targets[0]))
      mkdirSync(path.join(f.root, f.targets[0]))
      if (mode === 'both-read-failures') {
        rmSync(path.join(f.root, f.targets[1]))
        mkdirSync(path.join(f.root, f.targets[1]))
      }
    }
    let failure
    assert.throws(() => controller.restoreTestOverlays(f.root, f.evidence, originals), error => {
      failure = error
      return true
    })
    const errors = failure instanceof AggregateError ? failure.errors : [failure]
    assert.equal(errors[0].code, mode === 'first-original-hash-failure' ? 'ERR_ASSERTION' : 'EISDIR')
    if (mode === 'both-read-failures') {
      assert.equal(errors.length, 2)
      assert.equal(errors[1].code, 'EISDIR')
      assert.equal(failure.cause, errors[0])
    }
    else {
      assert.equal(errors.length, 1)
      assert.deepEqual(readFileSync(path.join(f.root, f.targets[1])), f.originals[f.targets[1]])
    }
    if (mode === 'first-preservation-failure')
      assert.equal(readFileSync(path.join(f.root, f.targets[0]), 'utf8'), 'UNKNOWN SOURCE BYTES MUST REMAIN')
    if (mode === 'first-original-hash-failure')
      assert.deepEqual(readFileSync(path.join(f.root, f.targets[0])), beforeFirst)
  })
}


for (const failure of ['binding-rejection', 'metadata-failure']) {
  test(`R86 actual recorder retains ${failure} across source document replacement and refuses partial evidence`, async (t) => {
    const f = await completeSourceReceipt(t, { returnFixture: true })
    const sufficientBeforeFailure = f.snapshot().journal
    assert.equal(projectDirectOwners(sufficientBeforeFailure, privateErrors, 0, completeClosure).receiptStatus, 'SOURCE_CONDITIONAL_PARTIAL')
    if (failure === 'binding-rejection') {
      f.context.__c40RecordDirectOwner = () => Promise.reject(new Error('controlled original binding failure'))
      f.context.failedLifecycle = f.callbacks.get('beforeunload')
      runInContext(`{
        const event = new BeforeUnloadEvent('beforeunload');
        Object.defineProperties(event, {isTrusted:{value:true},target:{value:document},currentTarget:{value:window}});
        failedLifecycle(event);
      }`, f.context)
      await setImmediate()
    }
    else {
      const original = Promise.resolve(Response.json({ original: true }))
      f.queue.push({ promise: original })
      const options = new Proxy({ method: 'POST', signal: f.signal() }, { getOwnPropertyDescriptor() { throw new Error('controlled metadata observation failure') } })
      assert.equal(f.fetch('/api/installations/private/direct/PRIVATE_CONNECTION_CANARY/signal', options), original)
      await original
    }
    assert.equal(f.snapshot().observerFailures > 0, true)
    // SOURCE-only replacement: a new bootstrap closure/document shares the real
    // journal storage adapter. No native navigation or generation correlation.
    runInContext("delete window.__c40DirectOwnerReceipt; document={defaultView:window};cairnAvailabilityDocument='REPLACEMENT_SOURCE_DOCUMENT';location.pathname='/health'", f.context)
    runInContext(`(${recorderBootstrapSource()})(${JSON.stringify(f.installationScope)})`, f.context)
    const afterReplacement = f.snapshot()
    assert.equal(afterReplacement.observerFailures > 0, true)
    const projected = projectDirectOwners(sufficientBeforeFailure, privateErrors, afterReplacement.observerFailures, completeClosure)
    assert.equal(projected.receiptStatus, 'UNKNOWN')
  })
}


function partialOriginalObjectView(f, selectedProduct) {
  const objects = path.join(f.root, 'owned-partial-git-objects')
  mkdirSync(objects)
  const env = controlledSourceGitEnvironment({ ...process.env, GIT_OBJECT_DIRECTORY: objects, GIT_ALTERNATE_OBJECT_DIRECTORIES: '', GIT_NO_LAZY_FETCH: '1' })
  const attributes = execFileSync('git', controlledSourceGitArgs(['rev-parse', `${selectedProduct}:.gitattributes`]), { encoding: 'utf8' }).trim()
  const entries = [[attributes, 'blob'], [selectedProduct, 'commit']]
  for (const tree of ['', ':tests', ':tests/e2e', ':apps', ':apps/web', ':apps/web/tests']) {
    const expression = tree ? selectedProduct + tree : selectedProduct + '^{tree}'
    entries.push([execFileSync('git', controlledSourceGitArgs(['rev-parse', expression]), { encoding: 'utf8' }).trim(), 'tree'])
  }
  entries.push([execFileSync('git', controlledSourceGitArgs(['rev-parse', `${selectedProduct}:${f.targets[1]}`]), { encoding: 'utf8' }).trim(), 'blob'])
  for (const [id, type] of entries) {
    const bytes = execFileSync('git', controlledSourceGitArgs(['cat-file', type, id]))
    const written = execFileSync('git', controlledSourceGitArgs(['hash-object', '-w', '-t', type, '--stdin']), { input: bytes, env, encoding: 'utf8' }).trim()
    assert.equal(written, id)
  }
  return env
}

for (const phase of ['restore', 'restore-unit']) {
  test(`H87 actual CLI ${phase} acquires only each chosen original and restores the available unit`, async (t) => {
    const f = sourceFiles(t)
    const controller = await restorationSource(f)
    controller.applyTestOverlays(f.root)
    const env = partialOriginalObjectView(f, controller.product)
    const missing = spawnSync('git', controlledSourceGitArgs(['show', `${controller.product}:${f.targets[0]}`]), { cwd: f.root, env, encoding: 'utf8' })
    assert.notEqual(missing.status, 0)
    const available = execFileSync('git', controlledSourceGitArgs(['show', `${controller.product}:${f.targets[1]}`]), { cwd: f.root, env })
    assert.deepEqual(available, f.originals[f.targets[1]])
    const result = spawnSync(process.execPath, [controller.sourceFile, phase], { cwd: f.root, env: { ...env, C40_EVIDENCE_DIR: f.evidence }, encoding: 'utf8' })
    // Missing full runtime-binding metadata keeps this deliberately partial
    // fixture RED; the assertion is the actual chosen-resource acquisition.
    assert.notEqual(result.status, 0)
    assert.deepEqual(readFileSync(path.join(f.root, f.targets[1])), f.originals[f.targets[1]])
    if (phase === 'restore-unit') {
      const receipt = JSON.parse(readFileSync(path.join(f.evidence, 'restore-unit-receipt.json'), 'utf8'))
      assert.equal(receipt.unitOnly, true)
      assert.equal(receipt.originalRestored, true)
    }
  })
}


for (const evidence of ['missing', 'corrupt']) {
  test(`R86 actual recorder refuses ${evidence} persistent failure evidence`, (t) => {
    const f = fixture(t)
    const key = '__c40_direct_owner_journal_v1:observer-failure'
    if (evidence === 'missing')
      f.storage.delete(key)
    else
      f.storage.set(key, 'CORRUPT_SOURCE_EVIDENCE')
    const snapshot = f.snapshot()
    assert.equal(snapshot.observerFailures > 0, true)
    assert.equal(projectDirectOwners(snapshot.journal, [], snapshot.observerFailures, completeClosure).receiptStatus, 'UNKNOWN')
  })
}


for (const failure of ['binding-rejection', 'journal-failure']) {
  test(`R87 actual recorder keeps ${failure} in host evidence when sticky persistence fails and document is replaced`, async (t) => {
    const page = new EventEmitter()
    page.exposeFunction = async () => {}
    page.addInitScript = async () => {}
    const host = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
    await host.install()
    t.after(() => host.dispose())
    const f = await completeSourceReceipt(t, { returnFixture: true, onFailureSignal: (...args) => {
      assert.equal(args.length, 1)
      page.emit('console', { type: () => 'error', text: () => args[0] })
    } })
    assert.equal(projectDirectOwners(f.snapshot().journal, privateErrors, 0, completeClosure).receiptStatus, 'SOURCE_CONDITIONAL_PARTIAL')
    f.faults.markerWrite = true
    f.faults.journalWrite = failure === 'journal-failure'
    if (failure === 'binding-rejection')
      f.context.__c40RecordDirectOwner = () => Promise.reject(new Error('controlled binding delivery failure'))
    f.callbacks.get('error')({ error: f.originalCause, isTrusted: true, timeStamp: 123 })
    await setImmediate()
    assert.equal(f.snapshot().observerFailures > 0, true)
    assert.equal(f.storage.get('__c40_direct_owner_journal_v1:observer-failure'), '0')
    f.faults.markerWrite = false
    f.faults.journalWrite = false
    runInContext("delete window.__c40DirectOwnerReceipt;document={defaultView:window};cairnAvailabilityDocument='REPLACEMENT_SOURCE_DOCUMENT';location.pathname='/health'", f.context)
    runInContext(`(${recorderBootstrapSource()})(${JSON.stringify(f.installationScope)})`, f.context)
    const replaced = f.snapshot()
    assert.equal(replaced.observerFailures, 0)
    assert.equal(projectDirectOwners(replaced.journal, privateErrors, 0, completeClosure).receiptStatus, 'SOURCE_CONDITIONAL_PARTIAL')
    host.accept(replaced)
    const evidence = host.finish(completeClosure)
    assert.equal(evidence.privateEvidence.observerFailures > 0, true)
    assert.equal(evidence.projection.receiptStatus, 'UNKNOWN')
  })
}


test('R87 host ingress accepts only the closed diagnostic code and detaches its owned listeners', async () => {
  const page = new EventEmitter()
  page.exposeFunction = async () => {}
  page.addInitScript = async () => {}
  const host = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
  await host.install()
  assert.equal(page.listenerCount('console'), 1)
  page.emit('console', { type: () => 'error', text: () => 'UNRELATED_PRIVATE_SOURCE_LOG_MUST_NOT_BE_COLLECTED' })
  assert.equal(host.finish(completeClosure).privateEvidence.observerFailures, 0)
  assert.equal(JSON.stringify(host.finish(completeClosure)).includes('UNRELATED_PRIVATE_SOURCE_LOG'), false)
  page.emit('console', { type: () => 'error', text: () => '__c40_direct_observer_failure_v1__' })
  assert.equal(host.finish(completeClosure).privateEvidence.observerFailures, 1)
  assert.equal(host.dispose(), true)
  assert.equal(page.listenerCount('console'), 0)
  assert.equal(page.listenerCount('pageerror'), 0)
})

test('R87 failure-ingress cleanup attempts both owned listeners independently and keeps unknown red', async (t) => {
  const page = new EventEmitter()
  page.exposeFunction = async () => {}
  page.addInitScript = async () => {}
  const host = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
  await host.install()
  const off = page.off.bind(page)
  page.off = (kind, listener) => {
    if (kind === 'pageerror')
      throw new Error('controlled pageerror detachment failure')
    return off(kind, listener)
  }
  t.after(() => { page.off = off; host.dispose() })
  assert.equal(host.dispose(), false)
  assert.equal(page.listenerCount('console'), 0)
  assert.equal(host.finish(completeClosure).privateEvidence.observerFailures > 0, true)
  assert.equal(host.finish(completeClosure).projection.receiptStatus, 'UNKNOWN')
})


test('R87 host cleanup retains both independent listener failures', async (t) => {
  const page = new EventEmitter()
  page.exposeFunction = async () => {}
  page.addInitScript = async () => {}
  const host = createDirectOwnerObserver(page, { installationId: 'private', installationSlug: 'private' })
  await host.install()
  const attempted = []
  const off = page.off.bind(page)
  t.after(() => { page.off = off; host.dispose() })
  page.off = (kind) => { attempted.push(kind); throw new Error('controlled listener cleanup failure') }
  assert.equal(host.dispose(), false)
  assert.deepEqual(attempted, ['pageerror', 'console'])
  assert.equal(host.finish(completeClosure).privateEvidence.observerFailures, 2)
  assert.equal(host.finish(completeClosure).projection.receiptStatus, 'UNKNOWN')
})


{
  const universalClaim = process.argv.includes('--reflection-side-effect-counterexample')
  test(`${universalClaim ? 'R90 UNIVERSAL TRANSPARENCY COUNTEREXAMPLE' : 'OUT_OF_DOMAIN_OPERATION_LIMIT_RECOGNITION'}: Direct reflective trap changes original native entry`, async (t) => {
    const f = fixture(t)
    const makeOptions = () => {
      const controller = new AbortController()
      const reason = new DOMException('observer-only Direct reflection SOURCE abort', 'AbortError')
      return { controller, options: new Proxy({ method: 'POST', signal: controller.signal }, {
        getPrototypeOf() { controller.abort(reason); return Object.prototype },
      }) }
    }
    const native = (_input, options) => options.signal.aborted ? Promise.reject(options.signal.reason) : Promise.resolve(Response.json({ available: false }))
    const baseline = makeOptions()
    f.queue.push(native)
    await Reflect.apply(f.nativeFetch, runInContext('window', f.context), ['/api/installations/private/direct/authorize', baseline.options])
    assert.equal(baseline.controller.signal.aborted, false)
    const observed = makeOptions()
    f.queue.push(native)
    const result = f.fetch('/api/installations/private/direct/authorize', observed.options)
    const rejected = await result.then(() => false, () => true)
    t.diagnostic(JSON.stringify({ category: universalClaim ? 'UNIVERSAL_OPERATION_COUNTEREXAMPLE' : 'OUT_OF_DOMAIN_OPERATION_LIMIT_RECOGNITION', universalOperationEquivalenceQualified: false, inputDomain: 'OUTSIDE_PINNED_PRODUCERS', baselineEnteredUnaborted: true, observedEnteredAborted: observed.controller.signal.aborted, originalDelegateCalls: f.requests.length, nativeBrowserExecuted: false }))
    assert.equal(f.requests.length, 2)
    assert.equal(rejected, !universalClaim, 'Universal equivalence is RED; recognition preserves the demonstrated out-of-domain interference')
  })
}


test('A90 actual chain validator refuses a universal operation claim and missing separate domain', () => {
  for (const mutate of [
    manifest => { manifest.sourceTrusted.universalOperationEquivalence = true },
    manifest => { delete manifest.sourceTrusted.domains.operation },
    manifest => { manifest.sourceTrusted.nativeReaderComposition.helper.commit = '0'.repeat(40) },
  ]) {
    const manifest = actualChainManifest()
    mutate(manifest)
    assert.throws(() => verifySourceTrustedChain(process.cwd(), path.dirname(fileURLToPath(import.meta.url)), manifest))
  }
})

// Git's own test switch enters its actual repository-ownership guard. This is a
// SOURCE process regression, not native navigation or a simulated JS Git policy.
function gitOwnershipFixtureEnvironment(f) {
  const global = path.join(f.root, 'empty-owned-global-git-config')
  writeFileSync(global, '')
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: global, GIT_CONFIG_COUNT: '0', GIT_TEST_ASSUME_DIFFERENT_OWNER: '1' }
  delete env.GIT_CONFIG_PARAMETERS
  return env
}

test('GIT OWNERSHIP real Git refusal, bounded acquisition, foreign repository refusal and original failure', (t) => {
  const f = sourceFiles(t)
  const env = gitOwnershipFixtureEnvironment(f)
  const setupEnv = { ...env, GIT_TEST_ASSUME_DIFFERENT_OWNER: '0' }
  const repositories = ['controlled-source', 'outside-source'].map(name => path.join(f.root, name))
  for (const root of repositories) {
    execFileSync('git', ['init', '--quiet', root], { env: setupEnv })
    const blob = execFileSync('git', ['-C', root, 'hash-object', '-w', '--stdin'], { env: setupEnv, input: 'SOURCE FIXTURE BYTES', encoding: 'utf8' }).trim()
    const tree = execFileSync('git', ['-C', root, 'mktree'], { env: setupEnv, input: `100644 blob ${blob}\tprobe.txt\n`, encoding: 'utf8' }).trim()
    const commit = execFileSync('git', ['-C', root, '-c', 'user.name=SOURCE Fixture', '-c', 'user.email=source-fixture@example.invalid', 'commit-tree', tree], { env: setupEnv, input: 'owned fixture\n', encoding: 'utf8' }).trim()
    execFileSync('git', ['-C', root, 'update-ref', 'HEAD', commit], { env: setupEnv })
  }
  const [controlled, foreign] = repositories
  const before = repositories.map(root => readFileSync(path.join(root, '.git/config')))
  const args = ['show', 'HEAD:probe.txt']
  const refused = spawnSync('git', args, { cwd: controlled, env, encoding: 'utf8' })
  assert.equal(refused.status, 128)
  assert.match(refused.stderr, /detected dubious ownership/)
  assert.equal(refused.stdout, '')
  const accepted = spawnSync('git', controlledSourceGitArgs(args, controlled), { cwd: controlled, env, encoding: 'utf8' })
  assert.equal(accepted.status, 0)
  assert.equal(accepted.stdout, 'SOURCE FIXTURE BYTES')
  const outside = spawnSync('git', controlledSourceGitArgs(args, controlled), { cwd: foreign, env, encoding: 'utf8' })
  assert.equal(outside.status, 128)
  assert.match(outside.stderr, /detected dubious ownership/)
  const delegatedEnv = controlledSourceGitEnvironment(env, controlled)
  assert.equal(env.GIT_CONFIG_COUNT, '0')
  assert.equal(delegatedEnv.GIT_CONFIG_COUNT, '1')
  assert.equal(delegatedEnv.GIT_CONFIG_VALUE_0, controlled)
  const delegated = spawnSync('git', args, { cwd: controlled, env: delegatedEnv, encoding: 'utf8' })
  assert.equal(delegated.status, 0)
  assert.equal(delegated.stdout, 'SOURCE FIXTURE BYTES')
  const foreignDelegated = spawnSync('git', args, { cwd: foreign, env: delegatedEnv, encoding: 'utf8' })
  assert.equal(foreignDelegated.status, 128)
  assert.match(foreignDelegated.stderr, /detected dubious ownership/)
  const missing = ['show', 'HEAD:missing-original']
  const original = spawnSync('git', missing, { cwd: controlled, env: setupEnv, encoding: 'utf8' })
  const preserved = spawnSync('git', controlledSourceGitArgs(missing, controlled), { cwd: controlled, env, encoding: 'utf8' })
  assert.equal(original.status, 128)
  assert.equal(preserved.status, original.status)
  assert.equal(preserved.signal, original.signal)
  assert.equal(preserved.stdout, original.stdout)
  assert.equal(preserved.stderr, original.stderr)
  for (let i = 0; i < repositories.length; i++)
    assert.deepEqual(readFileSync(path.join(repositories[i], '.git/config')), before[i])
  assert.equal(readFileSync(env.GIT_CONFIG_GLOBAL, 'utf8'), '')
  t.diagnostic(JSON.stringify({ actualGitOwnershipGuardRefused: true, sameBoundedSourceReadable: true, foreignRepositoryStillRefused: true, gitFailureStatusAndStderrPreserved: true, configFilesUnchanged: true, nativeQualified: false, actualUidChanged: false }))
})

test('GIT OWNERSHIP six ACTUAL H84/H87 fixture scenarios across the Git ownership guard', (t) => {
  const f = sourceFiles(t)
  const directory = path.join(f.root, 'controlled-acquisition-source')
  mkdirSync(directory)
  const reference = process.env.C40_GIT_ACQUISITION_REFERENCE
  if (reference)
    assert.match(reference, /^[a-f0-9]{40}$/)
  const files = ['direct-owner-probe.node.mjs', 'direct-owner-overlay-control.mjs', 'direct-owner-probe.mjs', 'closed-target-error.mjs', 'overlay-manifest.json', 'mcps.direct-owner.overlay.ts', 'direct-transport.seam.overlay.ts', 'selected-workflow.yaml']
  for (const file of files) {
    const sourcePath = file === 'selected-workflow.yaml' ? '.github/workflows/webkit-dependency-regression.yaml' : '.github/diagnostics/' + file
    const bytes = reference
      ? execFileSync('git', controlledSourceGitArgs(['show', `${reference}:${sourcePath}`]))
      : readFileSync(new URL('./' + file, import.meta.url))
    writeFileSync(path.join(directory, file), bytes)
  }
  const env = gitOwnershipFixtureEnvironment(f)
  delete env.C40_RESTORE_REFERENCE
  delete env.C40_RECORDER_REFERENCE
  const result = spawnSync(process.execPath, ['--test', '--test-reporter=tap', '--test-name-pattern=^(H84 actual final workflow shell|H87 actual CLI)', path.join(directory, 'direct-owner-probe.node.mjs')], { cwd: process.cwd(), env, encoding: 'utf8' })
  writeFileSync(path.resolve('test-results/source-only-direct-owner/git-ownership-child-' + (reference ?? 'candidate') + '.private.log'), result.stdout + '\nSTDERR\n' + result.stderr)
  const count = field => Number(result.stdout.match(new RegExp(`^# ${field} (\\d+)$`, 'm'))?.[1])
  t.diagnostic(JSON.stringify({ source: reference ?? 'CURRENT_CONTROLLED_SOURCE', actualSelectedScenarios: count('tests'), pass: count('pass'), fail: count('fail'), skipped: count('skipped'), subprocessExit: result.status, ownershipGuardEnforced: true, nativeQualified: false, actualUidChanged: false }))
  assert.equal(count('tests'), 6)
  assert.equal(count('skipped'), 0)
  assert.equal(result.signal, null)
  assert.equal(result.status, 0, 'The SAME six real fixture scenarios must run across the repository ownership guard')
  assert.equal(count('pass'), 6)
  assert.equal(count('fail'), 0)
})
