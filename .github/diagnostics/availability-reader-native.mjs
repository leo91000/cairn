import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { execFileSync } from 'node:child_process'
import { once } from 'node:events'
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { stripTypeScriptTypes } from 'node:module'
import { chromium, devices, errors, expect, webkit } from '@playwright/test'

import { acquireClosedTargetReference, isClosedTargetError, ownSdkResource } from './closed-target-error.mjs'

const output = 'test-results/reader-native'
await mkdir(output, { recursive: true })
const control = 'chromium-original-json-state-before-first-pagehide-only'
const paths = {
  gate: 'tests/e2e/native-response-gate.ts',
  transport: 'apps/web/src/availability-read.ts',
  shell: 'apps/web/src/BeaconApp.vue',
}
const sources = Object.fromEntries(await Promise.all(Object.entries(paths).map(async ([key, path]) => [key, await readFile(path, 'utf8')])))
// Versioned test-helper dependency; historical runtime fixture bytes stay frozen.
const nativeReaderCompositionBinding = {
  "compositionId": "H91_NATIVE_READER_COMPOSITION_V2",
  "helper": {
    "commit": "e81b99601d5abeec663dd0c7a1cde8b72b72b35d",
    "path": "tests/e2e/availability-probe.ts",
    "gitBlob": "fa8e4d1aa18dceb2750c2b71bb945e108822fd31",
    "sha256": "528e6098ade467928e691c3f377509ca131fba911ea9f05a791eadeabf2df626"
  },
  "historicalHelper": {
    "commit": "8835fa792c12633b1cca830659ebaf04a6cd5547",
    "path": "tests/e2e/availability-probe.ts",
    "gitBlob": "f72cd5102e901e17e77728d06834737552503748",
    "sha256": "5d315a1c12a2835838645d0804cb0dda5e8dfd8503eab8c963ac3d9442baae61"
  },
  "order": [
    "nativeDiagnostics",
    "installAvailabilityProbe"
  ],
  "qualification": "SOURCE_ONLY_NEW_COMPOSITION_NO_HISTORICAL_RUN_TRANSFER"
}
const helperBinding = nativeReaderCompositionBinding.helper
assert.equal(execFileSync('git', ['rev-parse', `${helperBinding.commit}:${helperBinding.path}`], { encoding: 'utf8' }).trim(), helperBinding.gitBlob)
sources.probe = execFileSync('git', ['show', `${helperBinding.commit}:${helperBinding.path}`], { encoding: 'utf8' })
const sourceHashes = Object.fromEntries(Object.entries(sources).map(([key, value]) => [key, createHash('sha256').update(value).digest('hex')]))
const acquiredPositiveEvidence = {
  "runId": 42618105477,
  "observer": "52fdc119a120c75580ff883ae4f1e90b43e2f18f",
  "product": "721b3028d0b5464ec2ba019a87697bfb460d67df",
  "artifactId": 13673846042,
  "artifactSha256": "b3f9a74e690495005f9afc641801a3e7ba5f211e60c9bc7d330714aa7648ccea",
  "originalRunConclusion": "failure",
  "fullEightCellRunComplete": false,
  "observedPositiveCells": [
    "chromium-owned-fetch",
    "webkit-owned-fetch",
    "webkit-owned-body"
  ],
  "runtimeSourceHashes": {
    "transport": "ce1137a3b2e13b31186ac5871678e5b05316a9a99349f388dd9ce197411f400b",
    "probe": "6f7a76ea28c5f3a3ff0bae411e9541d6ff93e5a7e178def116de4a8d914eecfa",
    "shell": "805ddd3d9fb33f0b1bea89ce1bd1860bedb8d8a0eb815355ac50f878e62b0bb1"
  },
  "originalGateSha256": "535feba4da7fefc2f78f08d664980d7d71959efb54849a26cd3499958ce0fa4f"
}
// Historical observations remain tied to their original immutable sources; none is replayed or credited here.
const acquiredNegativeEvidence = {
  "runId": 42620213392,
  "observer": "bcea1b2da05daf60427b35a48bfa6e91045c96e6",
  "product": "7df2ac2bb0f34fa917e64cc2fa928dcf5d1f5224",
  "artifactId": 13676760496,
  "artifactSha256": "2479ca417a480c67769eaf7a852a7ef9e2c573f6feed706398939ef60a91b127",
  "originalRunConclusion": "failure",
  "fullEightCellRunComplete": false,
  "observedNegativeCells": [
    "webkit-fetch-failure-first",
    "webkit-body-failure-first",
    "webkit-unread-503",
    "webkit-outside-call"
  ],
  "sourceHashes": {
    "gate": "2d7a23c328f0a8486f13ca7499abea475a4b127e505d26b1518e513ad92be467",
    "probe": "6f7a76ea28c5f3a3ff0bae411e9541d6ff93e5a7e178def116de4a8d914eecfa",
    "transport": "ce1137a3b2e13b31186ac5871678e5b05316a9a99349f388dd9ce197411f400b",
    "shell": "805ddd3d9fb33f0b1bea89ce1bd1860bedb8d8a0eb815355ac50f878e62b0bb1"
  }
}
const expectedCurrentHashes = {'gate': 'f5f8cc85a78a13f3c2af37eaa06cfc61641787b6fa65434d6283f70f1dcd8b95', 'probe': helperBinding.sha256, 'transport': '13593f5f5e928985d287e8b72fa667ce21deeb006dd724dc806c624bd52af727', 'shell': '805ddd3d9fb33f0b1bea89ce1bd1860bedb8d8a0eb815355ac50f878e62b0bb1'}
for (const [key, sha256] of Object.entries(expectedCurrentHashes))
  assert.equal(sourceHashes[key], sha256, 'Frozen selected source changed: ' + key)
const loadTypeScript = text => import('data:text/javascript;base64,' + Buffer.from(stripTypeScriptTypes(text)).toString('base64'))
const { nativeResponseGate } = await loadTypeScript(sources.gate)
const { installAvailabilityProbe, incompleteAvailabilityReads } = await loadTypeScript(sources.probe)
// Executed and source-tested as one composition, not just nativeDiagnostics alone.
async function installNativeReaderComposition(page, record, recordObserverFailure, mode) {
  const initializers = []
  await installAvailabilityProbe({
    exposeFunction: (...args) => page.exposeFunction(...args),
    on: (...args) => page.on(...args),
    once: (...args) => page.once(...args),
    off: (...args) => page.off(...args),
    addInitScript: callback => { initializers.push(callback.toString()) },
  }, record, recordObserverFailure)
  assert.equal(initializers.length, 1)
  await page.addInitScript(`(${nativeDiagnostics.toString()})(${JSON.stringify(mode)});(${initializers[0]})();`)
}
const pollStart = sources.shell.indexOf('let availabilityTimer:')
const pollEnd = sources.shell.indexOf('function installationStatus(', pollStart)
assert.ok(pollStart >= 0 && pollEnd > pollStart)
const polling = stripTypeScriptTypes(sources.shell.slice(pollStart, pollEnd))
const transport = stripTypeScriptTypes(sources.transport).replace(/^export /gm, '')
const body = [{ id: 'installation', online: true, updateRequired: false }]
const bootstrap = `window.startDiagnosticPoll = () => {
  const session = { value: { authenticated: true, installations: ${JSON.stringify(body)} } };
  const state = { installationId: 'installation', installationOnline: true, signingOut: false, redirecting: false };
  const watch = () => {};
  const onScopeDispose = callback => window.diagnosticDispose = callback;
  const redirect = () => { throw new Error('Unexpected authentication redirect'); };
  window.cairnExpectedAvailability = ${JSON.stringify(body)};
  ${transport}
  ${polling}
  window.diagnosticState = () => ({ online: state.installationOnline, failures: availabilityFailures, suspended: availabilitySuspended, requestActive: availabilityRequest !== undefined });
  void refreshAvailability();
};`
const planned = [
  { engine: 'chromium', mode: 'pre-pagehide-original-body' },
]
const rows = []
const cleanupFailures = []
const engineFailures = []
const errorRecord = error => ({ name: error?.name ?? typeof error, type: error?.constructor?.name ?? typeof error, message: String(error?.message ?? error) })
const write = (name, value) => writeFile(output + '/' + name, JSON.stringify(value, null, 2))
async function observed(operation, label) {
  let timer
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Observation deadline: ' + label)), 7000) }),
    ])
  }
  finally { clearTimeout(timer) }
}

// Negative adapters alter only their declared boundary. All original promises,
// Response objects and native JSON results are returned unchanged.
function nativeDiagnostics(mode) {
  'use strict'

  let read = 0
  const fetch = window.fetch
  const abort = AbortController.prototype.abort
  const json = Response.prototype.json
  const responses = new WeakMap()
  const snapshots = []
  let channel
  const snapshot = (trigger, selectedRead) => {
    snapshots.push({ trigger, read: selectedRead })
    if (channel)
      return
    channel = new MessageChannel()
    channel.port1.onmessage = () => {
      const batch = snapshots.splice(0)
      channel.port1.close(); channel.port2.close(); channel = undefined
      const receipt = {
        document: window.cairnAvailabilityDocument, pendingSnapshots: snapshots.length,
        state: window.diagnosticState?.() ?? null,
        journal: JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]'),
        native: JSON.parse(sessionStorage.getItem('cairn-native-fetch-journal') || '[]'),
      }
      for (const selected of batch)
        void window.nativeStateReceipt({ ...receipt, ...selected })
    }
    channel.port2.postMessage(null)
  }
  const pagehideEntry = function diagnosticPagehideEntry() {
    if (channel) { channel.port1.close(); channel.port2.close(); channel = undefined }
    snapshots.length = 0
  }
  window.cairnNativePagehideEntry = pagehideEntry
  window.addEventListener('pagehide', pagehideEntry)
  const record = (kind, extra = {}) => {
    const journal = JSON.parse(sessionStorage.getItem('cairn-native-fetch-journal') || '[]')
    journal.push({ kind, sequence: journal.length + 1, ...extra })
    sessionStorage.setItem('cairn-native-fetch-journal', JSON.stringify(journal))
  }
  // Test-only admission control: cancel within native shouldClose(), BEFORE
  // the engine stops this original request's document loader. Production has no prompt.
  if (mode === 'proof-without-terminal') {
    window.addEventListener('beforeunload', event => {
      if (event.isTrusted) {
        event.preventDefault()
        event.returnValue = 'diagnostic-only'
      }
    })
  }
  const ordinaryData = (value) => {
    if (value == null)
      return Object.create(null)
    if (typeof value !== 'object')
      return null
    const prototype = Object.getPrototypeOf(value)
    if (prototype !== null && prototype !== Object.prototype)
      return null
    if (prototype) {
      const standard = ['constructor', '__defineGetter__', '__defineSetter__', 'hasOwnProperty', '__lookupGetter__', '__lookupSetter__', 'isPrototypeOf', 'propertyIsEnumerable', 'toString', 'valueOf', '__proto__', 'toLocaleString']
      const inherited = Object.getOwnPropertyDescriptors(prototype)
      Object.setPrototypeOf(inherited, null)
      for (const key of Reflect.ownKeys(inherited)) {
        if (typeof key !== 'string' || !standard.includes(key))
          return null
        const descriptor = Object.getOwnPropertyDescriptor(inherited, key).value
        if (key !== '__proto__' && !Object.hasOwn(descriptor, 'value'))
          return null
      }
    }
    const descriptors = Object.getOwnPropertyDescriptors(value)
    Object.setPrototypeOf(descriptors, null)
    const data = Object.create(null)
    for (const key of Reflect.ownKeys(descriptors)) {
      if (typeof key !== 'string')
        return null
      const descriptor = Object.getOwnPropertyDescriptor(descriptors, key).value
      if (!Object.hasOwn(descriptor, 'value'))
        return null
      data[key] = descriptor.value
    }
    return data
  }
  const requestData = (value) => {
    const data = ordinaryData(value)
    if (!data)
      return null
    for (const [key, item] of Object.entries(data)) {
      if (key === 'signal')
        continue
      if (key === 'headers' && item != null) {
        const headers = ordinaryData(item)
        if (!headers || Object.values(headers).some(value => typeof value !== 'string'))
          return null
      }
      else if (item !== null && (typeof item === 'object' || typeof item === 'function'))
        return null
    }
    return data
  }

  const signals = new WeakMap()
  const failureConsole = window.console
  const failureConsoleError = failureConsole.error
  const observationFailed = () => {
    try { Reflect.apply(failureConsoleError, failureConsole, ['__c40_archived_observer_failure_v1__']) } catch {}
    try { record('observer-failed') } catch {}
  }
  let controllerPrototype = AbortController.prototype
  let controllerSignal
  while (controllerPrototype && !controllerSignal) {
    controllerSignal = Object.getOwnPropertyDescriptor(controllerPrototype, 'signal')?.get
    controllerPrototype = Object.getPrototypeOf(controllerPrototype)
  }
  AbortController.prototype.abort = function (...args) {
    let target
    try {
      if (controllerSignal)
        target = signals.get(Reflect.apply(controllerSignal, this, []))
      else
        observationFailed()
    }
    catch { observationFailed() }
    if (mode === 'outside-call' && target === 2) {
      queueMicrotask(() => Reflect.apply(abort, this, args))
      try { record('deferred-owned-call-return', { read: target }) }
      catch { observationFailed() }
      return
    }
    return Reflect.apply(abort, this, args)
  }
  window.fetch = function (...args) {
    // Only the declared proof-without-terminal negative changes a wire signal.
    // Ordinary observation forwards the exact original argument list.
    if (typeof args[0] !== 'string') {
      const original = Reflect.apply(fetch, this, args)
      observationFailed()
      return original
    }
    if (args[0] !== '/api/installations')
      return Reflect.apply(fetch, this, args)
    const current = ++read
    let signal
    let metadataKnown = true
    try {
      const data = requestData(args[1])
      metadataKnown = data !== null
      signal = data?.signal
    }
    catch { metadataKnown = false }
    let request = args
    if (mode === 'proof-without-terminal' && current === 2 && metadataKnown) {
      const wire = new AbortController()
      record('negative-wire-substitution', { read: current, distinctSignal: wire.signal !== signal })
      request = [...args]
      request[1] = { ...args[1], signal: wire.signal }
    }
    const original = Reflect.apply(fetch, this, request)
    if (!metadataKnown) {
      observationFailed()
      return original
    }
    try {
      if (signal) {
        signals.set(signal, current)
        signal.addEventListener('abort', event => {
          try {
            record('private-native-signal', {
              read: current, trusted: event.isTrusted,
              exactTarget: event.target === signal && event.currentTarget === signal && event instanceof Event,
              aborted: signal.aborted,
            })
            snapshot('signal', current)
          }
          catch { observationFailed() }
        })
      }
      void original.then(response => {
        try { responses.set(response, current); snapshot('fetch-fulfilled', current) }
        catch { observationFailed() }
      }, () => {
        try { snapshot('fetch-rejected', current) }
        catch { observationFailed() }
      })
    }
    catch { observationFailed() }
    return original
  }

  Response.prototype.json = function (...args) {
    const original = Reflect.apply(json, this, args)
    const current = responses.get(this)
    if (current) {
      // Fixture reference only: preserve and return the ORIGINAL native promise.
      // CDP reads its internal state while paused BEFORE the first pagehide callback.
      if (mode === 'pre-pagehide-original-body' && current === 3)
        window.cairnNativeOriginalJSON = original
      void original.then(() => snapshot('body', current), () => snapshot('body-rejected', current))
    }
    return original
  }
}

// A page-scoped CDP session is retired by its owned page, not by commands
// that race that page's destruction. The actual session-close event is required.
function ownPageCdp(context, page, pageOwner, trackCdp, onClosed) {
  return ownSdkResource(async () => {
    const session = await trackCdp('newCDPSession', () => context.newCDPSession(page))
    const closed = Promise.withResolvers()
    session.once('close', () => {
      onClosed()
      closed.resolve()
    })
    return { session, closed: closed.promise }
  }, async (resource) => {
    await pageOwner.release()
    await resource.closed
  })
}

// Derive pause provenance from the paused frame and reported live context.
// Expected scope is a comparison target, never a substitute for missing data.
function identifyPause(event, expected, scripts, contexts) {
  const frame = event.callFrames?.[0]
  const scriptId = frame?.location?.scriptId
  const script = scripts.get(scriptId)
  const context = script && contexts.get(script.executionContextId)
  const actual = {
    callFrameId: frame?.callFrameId ?? null,
    scriptId: scriptId ?? null,
    functionLocation: frame?.functionLocation ?? null,
    executionContextId: script?.executionContextId ?? null,
    uniqueId: context?.uniqueId ?? null,
    frameId: context?.auxData?.frameId ?? null,
    defaultContext: context?.auxData?.isDefault ?? null,
    origin: context?.origin ?? null,
  }
  const result = { status: 'unknown', actual, entryMatched: false }
  if (!actual.callFrameId || !script || !context || !actual.functionLocation || !script.uniqueId)
    return result
  const locationMatches = actual.functionLocation.scriptId === expected.entry.scriptId
    && actual.functionLocation.lineNumber === expected.entry.lineNumber
    && actual.functionLocation.columnNumber === expected.entry.columnNumber
  const contextMatches = actual.executionContextId === expected.context.id
    && actual.uniqueId === expected.context.uniqueId && script.uniqueId === actual.uniqueId
    && actual.frameId === expected.frameId && actual.defaultContext === true
    && actual.origin === expected.origin
  result.entryMatched = locationMatches && actual.scriptId === expected.entry.scriptId
  result.status = contextMatches && result.entryMatched ? 'matching-frame-context' : 'wrong'
  return result
}

async function observe(browser, cell, closedPrototype) {
  const arm = cell.engine + '-' + cell.mode
  const received = []
  const stateReceipts = []
  const dialogs = []
  const dialogActions = new Set()
  const pageErrors = []
  let hostObserverFailures = 0
  const assertArchiveObserver = () => {
    if (hostObserverFailures > 0)
      throw new Error('Archived observation failure makes evidence UNKNOWN')
  }
  const failures = []
  const writes = new Set()
  const gates = []
  let phase = 'setup'
  let context, page, gate, navigation, documentId, initial, destination, next, outage, contextOwner, pageOwner, gateOwner
  let cdp, cdpOwner, cdpAdmission, originalPromiseBoundary, debuggerAction
  let cdpCloseObserved = false
  let cleanupStartedAt, postCommitReceipt
  const pauseReceipts = []
  const cdpTasks = new Set()
  const cdpTaskRecords = []
  const trackCdp = (method, operation) => {
    const record = { method, state: 'pending' }
    cdpTaskRecords.push(record)
    const pending = Promise.resolve().then(operation)
    cdpTasks.add(pending)
    void pending.then(() => {
      record.state = 'fulfilled'
      cdpTasks.delete(pending)
    }, () => {
      record.state = 'rejected'
      cdpTasks.delete(pending)
    })
    return pending
  }
  const sendCdp = (method, params) => trackCdp(method, () => cdp.send(method, params))
  let outageEvidence
  let checkpoint, nativeEvents = [], initialClosureBeforeRelease = false, journalAcknowledged = false, navigationValidated = false
  let initialHeaderAdvance, initialClosureBoundary, nativeNavigationCheckpoint, navigationOutcome, rawNavigationError, navigationRetirement
  let activatedViaClick = false, retainedAfterDialog = false, pageCloseAttempted = false, pageCloseCompleted = false, waiterPendingAtCleanup = false
  let pendingProof, state, finalAudit = { unknown: [], unexpected: [] }
  const remember = (stage, error) => failures.push({ stage, error: errorRecord(error) })
  const cleanupFailure = (actor, error) => cleanupFailures.push({ arm, actor, error: errorRecord(error) })
  const stage = async name => {
    phase = name
    console.log(JSON.stringify({ control, arm, phase }))
    await write('active.json', { control, arm, phase, received, pageErrors, failures, complete: false })
  }
  const record = event => {
    if (!received.some(item => item.document === event.document && item.sequence === event.sequence))
      received.push(event)
    const pending = appendFile(output + '/' + arm + '.reads.jsonl', JSON.stringify(event) + '\n')
    writes.add(pending)
    void pending.catch(error => cleanupFailure('receipt-write', error)).finally(() => writes.delete(pending))
  }
  const persistEvidence = (name, value) => {
    const pending = write(name, value)
    writes.add(pending)
    void pending.then(() => writes.delete(pending), () => writes.delete(pending))
    return observed(() => pending, 'persist ' + name)
  }
  const evaluate = (fn, value) => observed(() => page.evaluate(fn, value), 'evaluate: ' + phase)
  const events = read => received.filter(event => event.document === documentId && event.read === read)
  const terminal = read => events(read).filter(event => ['rejected', 'body-rejected', 'body'].includes(event.kind))
  const poll = operation => expect.poll(() => { gate.check(); return operation() }, { timeout: 7000 }).toBe(true)
  const addGate = (label, response) => { gates.push({ label, response }); return response }
  const stateReceipt = async (read, trigger) => {
    await poll(() => stateReceipts.some(item => item.document === documentId && item.read === read
      && item.trigger === trigger && item.pendingSnapshots === 0 && item.state !== null))
    return stateReceipts.findLast(item => item.document === documentId && item.read === read
      && item.trigger === trigger && item.pendingSnapshots === 0 && item.state !== null)
  }
  const harvest = async () => {
    checkpoint = await evaluate(() => ({
      document: window.cairnAvailabilityDocument,
      journal: JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]'),
      native: JSON.parse(sessionStorage.getItem('cairn-native-fetch-journal') || '[]'),
    }))
    for (const event of checkpoint.journal) {
      if (!received.some(item => item.document === event.document && item.sequence === event.sequence))
        received.push(event)
    }
    nativeEvents = checkpoint.native
  }
  let availabilityCount = 0
  const backend = createServer((request, response) => {
    response.setHeader('cache-control', 'no-store')
    if (request.url === '/api/installations') {
      availabilityCount++
      const unavailable = cell.mode === 'unread-503' && availabilityCount === 2
      response.writeHead(unavailable ? 503 : 200, { 'content-type': 'application/json' }).end(JSON.stringify(unavailable ? { error: 'genuine HTTP503' } : body))
    }
    else if (request.url === '/live')
      response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Live</h1><script src="/client.js"></script>')
    else if (request.url === '/client.js')
      response.writeHead(200, { 'content-type': 'application/javascript' }).end(bootstrap)
    else
      response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Destination</h1>')
  })
  try {
    backend.listen(0, '127.0.0.1')
    await observed(() => once(backend, 'listening'), 'backend listening')
    const origin = 'http://127.0.0.1:' + backend.address().port
    gateOwner = ownSdkResource(() => nativeResponseGate(origin), value => value.close())
    gate = await observed(() => gateOwner.result, 'gate setup')
    const { defaultBrowserType: _, ...device } = devices['Desktop Chrome']
    contextOwner = ownSdkResource(() => browser.newContext({ ...device, isMobile: true, hasTouch: true, proxy: { server: gate.server } }), value => value.close())
    context = await observed(() => contextOwner.result, 'context setup')
    context.setDefaultTimeout(7000)
    context.setDefaultNavigationTimeout(30000)
    await observed(() => context.tracing.start({ screenshots: false, snapshots: true, sources: true }), 'trace setup')
    pageOwner = ownSdkResource(() => context.newPage(), async (value) => { pageCloseAttempted = true; await value.close(); pageCloseCompleted = true })
    page = await observed(() => pageOwner.result, 'page setup')
    const archiveFailureListener = (message) => {
      if (message.type() === 'error' && message.text() === '__c40_archived_observer_failure_v1__')
        hostObserverFailures++
    }
    page.on('console', archiveFailureListener)
    page.once('close', () => page.off('console', archiveFailureListener))
    page.on('pageerror', error => pageErrors.push(errorRecord(error)))
    page.on('dialog', dialog => {
      const receipt = { type: dialog.type(), dismissed: false }
      dialogs.push(receipt)
      if (cell.mode !== 'proof-without-terminal' || dialog.type() !== 'beforeunload')
        remember('unexpected native dialog', new Error('Unexpected native dialog type'))
      const action = (async () => {
        try {
          await observed(() => dialog.dismiss(), 'native beforeunload dismissal')
          receipt.dismissed = true
        }
        catch (error) { cleanupFailure('native-dialog-dismissal', error) }
      })()
      dialogActions.add(action)
      void action.finally(() => dialogActions.delete(action))
    })

    await page.exposeFunction('nativeStateReceipt', receipt => {
      stateReceipts.push(receipt)
      for (const event of receipt.journal) {
        if (!received.some(item => item.document === event.document && item.sequence === event.sequence))
          received.push(event)
      }
      nativeEvents = receipt.native
      const pending = appendFile(output + '/' + arm + '.states.jsonl', JSON.stringify(receipt) + '\n')
      writes.add(pending)
      void pending.catch(error => cleanupFailure('state-write', error)).finally(() => writes.delete(pending))
    })
    await installNativeReaderComposition(page, record, () => { hostObserverFailures++ }, cell.mode)
    await stage('same actual poll and clock seed')
    await page.clock.install()
    await page.goto(origin + '/live')
    await poll(() => received.some(event => event.kind === 'pageshow' && event.trusted))
    documentId = await evaluate(() => window.cairnAvailabilityDocument)
    await evaluate(() => window.startDiagnosticPoll())
    await poll(() => events(1).some(event => event.kind === 'body' && event.exactBody))
    await page.clock.pauseAt(new Date(Date.now() + 1000))
    if (cell.mode === 'pre-pagehide-original-body') {
      await stage('pre-admit CDP before retained navigation while clock is paused')
      // Admit instrumentation while the existing product clock is paused,
      // BEFORE any retained navigation. The original native producer is untouched.
      cdpAdmission = { document: documentId, runtimeAck: false, frameAck: false, debuggerAck: false, breakpointAck: false, readyBeforeRetainedNavigation: false }
      await write(arm + '.cdp-admission.json', cdpAdmission)
      cdpOwner = ownPageCdp(context, page, pageOwner, trackCdp, () => { cdpCloseObserved = true })
      const cdpResource = await observed(() => cdpOwner.result, 'CDP acquisition')
      cdp = cdpResource.session
      const executionContexts = new Map()
      const scriptScopes = new Map()
      cdp.on('Runtime.executionContextCreated', event => executionContexts.set(event.context.id, event.context))
      cdp.on('Runtime.executionContextDestroyed', event => executionContexts.delete(event.executionContextId))
      cdp.on('Runtime.executionContextsCleared', () => executionContexts.clear())
      cdp.on('Debugger.scriptParsed', event => {
        const context = executionContexts.get(event.executionContextId)
        scriptScopes.set(event.scriptId, { executionContextId: event.executionContextId, uniqueId: context?.uniqueId ?? null })
      })
      await observed(() => sendCdp('Runtime.enable'), 'runtime contexts')
      cdpAdmission.runtimeAck = true
      await write(arm + '.cdp-admission.json', cdpAdmission)
      const frame = await observed(() => sendCdp('Page.getFrameTree'), 'owned main frame identity')
      const currentContexts = [...executionContexts.values()].filter(context => context.auxData?.isDefault === true
        && context.auxData.frameId === frame.frameTree.frame.id && context.origin === origin)
      assert.equal(currentContexts.length, 1)
      const currentContext = currentContexts[0]
      assert.ok(currentContext.uniqueId)
      cdpAdmission.frameAck = true
      cdpAdmission.context = { uniqueId: currentContext.uniqueId, frameId: frame.frameTree.frame.id, defaultContext: true, exactOrigin: true }
      await write(arm + '.cdp-admission.json', cdpAdmission)
      await observed(() => sendCdp('Debugger.enable'), 'debugger enable')
      cdpAdmission.debuggerAck = true
      await write(arm + '.cdp-admission.json', cdpAdmission)
      // Bind the first registered fixture listener to its actual function location.
      // This does not read or replace the native JSON promise.
      const entry = await observed(() => sendCdp('Runtime.evaluate', {
        uniqueContextId: currentContext.uniqueId,
        expression: 'globalThis.cairnNativePagehideEntry',
        returnByValue: false, silent: true, throwOnSideEffect: true,
      }), 'registered pagehide entry reference')
      assert.equal(entry.exceptionDetails, undefined)
      assert.equal(entry.result.type, 'function')
      assert.ok(entry.result.objectId)
      const entryProperties = await observed(() => sendCdp('Runtime.getProperties', {
        objectId: entry.result.objectId, ownProperties: true,
      }), 'registered pagehide entry location')
      assert.equal(entryProperties.exceptionDetails, undefined)
      const entryLocation = entryProperties.internalProperties?.find(item => item.name === '[[FunctionLocation]]')?.value?.value
      assert.equal(typeof entryLocation?.scriptId, 'string')
      assert.equal(Number.isInteger(entryLocation?.lineNumber), true)
      assert.equal(Number.isInteger(entryLocation?.columnNumber), true)
      const entryScope = scriptScopes.get(entryLocation.scriptId)
      assert.equal(entryScope?.executionContextId, currentContext.id)
      assert.equal(entryScope?.uniqueId, currentContext.uniqueId)
      cdpAdmission.entry = entryLocation
      await write(arm + '.cdp-admission.json', cdpAdmission)
      const expectedPause = { context: currentContext, frameId: frame.frameTree.frame.id, origin, entry: entryLocation }

      const paused = Promise.withResolvers()
      debuggerAction = paused.promise
      cdp.once('Debugger.paused', event => {
        const ingress = {
          ingressAtMs: performance.now(), phase, reason: event.reason ?? null,
          eventName: event.data?.eventName ?? null, expectedDocument: documentId,
          observedDocument: null, bodyHeldAtIngress: Boolean(next && !next.bodyReleased),
          cleanupStartedAtIngress: cleanupStartedAt ?? null,
          provenance: identifyPause(event, expectedPause, scriptScopes, executionContexts),
          status: 'unverified',
        }
        pauseReceipts.push(ingress)
        void (async () => {
          let observationError
          try {
            await persistEvidence(arm + '.pause-ingress.json', ingress)
            assert.equal(ingress.reason, 'EventListener')
            assert.equal(ingress.eventName, 'listener:pagehide')
            assert.equal(ingress.provenance.status, 'matching-frame-context')
            assert.equal(cleanupStartedAt, undefined)
            assert.equal(ingress.bodyHeldAtIngress, true)
            assert.equal(next.bodyReleased, false)
            const pausedDocument = await observed(() => sendCdp('Debugger.evaluateOnCallFrame', {
              callFrameId: ingress.provenance.actual.callFrameId,
              expression: 'globalThis.cairnAvailabilityDocument',
              returnByValue: true, silent: true, throwOnSideEffect: true,
            }), 'paused frame document identity')
            assert.equal(pausedDocument.exceptionDetails, undefined)
            ingress.observedDocument = pausedDocument.result.value ?? null
            ingress.status = ingress.observedDocument === documentId ? 'verified-document' : 'wrong-document'
            await persistEvidence(arm + '.pause-ingress.json', ingress)
            assert.equal(ingress.status, 'verified-document')
            assert.equal(cleanupStartedAt, undefined)
            assert.equal(next.bodyReleased, false)
            await observed(() => sendCdp('DOMDebugger.removeEventListenerBreakpoint', { eventName: 'pagehide' }), 'remove pagehide breakpoint')
            const target = await observed(() => sendCdp('Debugger.evaluateOnCallFrame', {
              callFrameId: ingress.provenance.actual.callFrameId,
              expression: 'globalThis.cairnNativeOriginalJSON',
              returnByValue: false, silent: true, throwOnSideEffect: true,
            }), 'same paused frame original native JSON promise')
            assert.equal(target.exceptionDetails, undefined)
            assert.equal(target.result.subtype, 'promise')
            assert.ok(target.result.objectId)
            const properties = await observed(() => sendCdp('Runtime.getProperties', {
              objectId: target.result.objectId, ownProperties: true,
            }), 'original native promise internal state')
            assert.equal(properties.exceptionDetails, undefined)
            assert.equal(cleanupStartedAt, undefined)
            assert.equal(next.bodyReleased, false)
            const stillCurrent = identifyPause(event, expectedPause, scriptScopes, executionContexts)
            assert.equal(stillCurrent.status, 'matching-frame-context')
            const internal = properties.internalProperties || []
            const state = internal.find(item => item.name === '[[PromiseState]]')?.value
            const result = internal.find(item => item.name === '[[PromiseResult]]')?.value
            originalPromiseBoundary = {
              beforeFirstListenerBody: ingress.provenance.entryMatched && ingress.reason === 'EventListener',
              reason: ingress.reason, eventName: ingress.eventName,
              exactOwnedContext: stillCurrent.status === 'matching-frame-context' && ingress.status === 'verified-document',
              document: ingress.observedDocument, read: 3, promiseState: state?.value ?? null,
              resultSubtype: result?.subtype ?? null, resultClassName: result?.className ?? null,
              originalBodyHeld: !next.bodyReleased, pauseIngressAtMs: ingress.ingressAtMs,
              cancellationEvidence: false, productQualified: false,
            }
            await persistEvidence(arm + '.original-promise-boundary.json', originalPromiseBoundary)
          }
          catch (error) { ingress.status = ingress.status === 'unverified' ? 'refused' : ingress.status; observationError = error }
          finally {
            try { await observed(() => sendCdp('Debugger.resume'), 'resume first pagehide callback') }
            catch (error) { observationError ||= error }
          }
          if (observationError) paused.reject(observationError)
          else paused.resolve()
        })()
      })
      // Avoid an unhandled observer rejection while the host awaits commit.
      void debuggerAction.catch(() => {})
      await observed(() => sendCdp('DOMDebugger.setEventListenerBreakpoint', { eventName: 'pagehide' }), 'arm first pagehide breakpoint')
      cdpAdmission.breakpointAck = true
      cdpAdmission.readyBeforeRetainedNavigation = true
      await write(arm + '.cdp-admission.json', cdpAdmission)
      await stage('same actual poll and clock seed')
    }
    initial = addGate('initial', ['owned-body', 'body-failure-first', 'unread-503'].includes(cell.mode) ? gate.holdBody() : gate.hold('availability'))
    await page.clock.runFor(3100)
    await poll(() => initial.ready)
    await poll(() => events(2).some(event => event.kind === 'start'))
    if (['owned-body', 'body-failure-first'].includes(cell.mode))
      await poll(() => events(2).some(event => event.kind === 'body-start'))
    await page.clock.resume()
    assert.equal(initial.status, cell.mode === 'unread-503' ? 503 : 200)

    if (cell.mode === 'unread-503') {
      await stage('real HTTP503 with unread body and caller-owned wire disposal')
      await poll(() => events(2).some(event => event.kind === 'fetch-fulfilled' && event.status === 503 && !event.ok))
      await poll(() => events(2).some(event => event.kind === 'abort' && event.trusted && event.exactTarget && event.insideAbortCall))
      await poll(() => initial.closed && initial.terminal === 'cancelled' && !initial.bodyReleased)
      initialClosureBeforeRelease = true
      state = (await stateReceipt(2, 'signal')).state
      assert.equal(state.online, false)
      assert.equal(state.failures, 1)
      assert.equal(events(2).some(event => ['body-start', 'body', 'body-rejected', 'rejected'].includes(event.kind)), false)
      navigationValidated = true // This cell deliberately performs no navigation.
    }
    else {
      if (cell.mode.endsWith('failure-first')) {
        await stage('real transport failure settles before any departure intent')
        if (cell.mode === 'fetch-failure-first') {
          outage = gate.failAvailabilityReads()
          outageEvidence = { attemptsBeforeFault: gate.availabilityRequests, rejectedRequests: null, restoredBeforeIntent: false }
        }
        initial.disconnect()
        await poll(() => terminal(2).length === 1)
        assert.equal(terminal(2)[0].kind, cell.mode === 'fetch-failure-first' ? 'rejected' : 'body-rejected')
        assert.equal(terminal(2)[0].aborted, false)
        assert.equal(terminal(2)[0].errorName, 'TypeError')
        state = (await stateReceipt(2, cell.mode === 'fetch-failure-first' ? 'fetch-rejected' : 'body-rejected')).state
        assert.equal(state.online, false)
        assert.equal(state.failures, 1)
        await poll(() => initial.closed && initial.terminal === 'cancelled' && !initial.bodyReleased)
        initialClosureBeforeRelease = true
        if (outage) {
          outageEvidence.rejectedRequests = outage.rejectedRequests
          outageEvidence.attemptsAfterTerminal = gate.availabilityRequests
          outage.restore()
          outageEvidence.restoredBeforeIntent = true
        }
      }
      destination = addGate('destination', gate.hold('agents'))
      if (cell.mode.startsWith('owned-') || cell.mode === 'pre-pagehide-original-body')
        next = addGate('replacement', gate.hold('availability'))
      if (cell.mode === 'proof-without-terminal') {
        await stage('activate test-only native beforeunload confirmation')
        await observed(() => page.getByRole('heading', { name: 'Live', exact: true }).click(), 'trusted activation click')
        activatedViaClick = true
      }
      if (cell.mode === 'pre-pagehide-original-body')
        assert.equal(cdpAdmission?.readyBeforeRetainedNavigation, true)
      await stage('original Page.goto with destination response held')
      navigation = page.goto(origin + '/installations/fixture/agents').then(() => {
        navigationOutcome = { kind: 'committed' }
        return navigationOutcome
      }, error => {
        rawNavigationError = error
        navigationOutcome = { kind: 'rejected', error: errorRecord(error), observedAfterPageCloseStarted: pageCloseAttempted }
        return navigationOutcome
      })
      if (cell.mode === 'proof-without-terminal') {
        await stage('post-dismissal native retained-document checkpoint before releasing original HTTP')
        await poll(() => dialogs.length === 1 && dialogs[0].type === 'beforeunload' && dialogs[0].dismissed)
        // The native UI decision and a fresh current-context receipt establish
        // this boundary. A pending Playwright goto waiter is not its native state.
        nativeNavigationCheckpoint = await evaluate(() => {
          const heading = document.querySelector('h1')
          const rect = heading?.getBoundingClientRect()
          return {
            document: window.cairnAvailabilityDocument, read: 2, trigger: 'post-native-dismissal',
            liveVisible: Boolean(heading?.textContent === 'Live' && rect && rect.width > 0 && rect.height > 0 && getComputedStyle(heading).visibility === 'visible'),
            livePath: location.pathname === '/live',
            state: window.diagnosticState(),
            journal: JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]'),
            native: JSON.parse(sessionStorage.getItem('cairn-native-fetch-journal') || '[]'),
          }
        })
        for (const event of nativeNavigationCheckpoint.journal) {
          if (!received.some(item => item.document === event.document && item.sequence === event.sequence))
            received.push(event)
        }
        nativeEvents = nativeNavigationCheckpoint.native
        assert.equal(nativeNavigationCheckpoint.document, documentId)
        assert.equal(nativeNavigationCheckpoint.liveVisible, true)
        assert.equal(nativeNavigationCheckpoint.livePath, true)
        assert.equal(received.some(event => event.document === documentId && event.kind === 'pagehide'), false)
        assert.equal(nativeNavigationCheckpoint.journal.some(event => event.document === documentId && event.kind === 'pagehide'), false)
        assert.equal(destination.received, false)
        assert.equal(destination.ready, false)
        retainedAfterDialog = true
        navigationValidated = true
      }
      else {
        await poll(() => destination.ready)
        assert.equal(destination.status, 200)
      }
      if (cell.mode.endsWith('failure-first')) {
        assert.equal(events(2).some(event => event.kind === 'abort'), false)
      }
      else if (cell.mode === 'proof-without-terminal') {
        await stage('owned signal alone cannot settle the original held fetch')
        await poll(() => events(2).some(event => event.kind === 'abort' && event.trusted && event.exactTarget && event.insideAbortCall))
        pendingProof = nativeNavigationCheckpoint
        assert.equal(pendingProof.state.requestActive, true)
        assert.equal(pendingProof.state.online, true)
        assert.equal(pendingProof.state.failures, 0)
        assert.equal(pendingProof.journal.some(event => event.document === documentId && event.read === 2 && ['headers', 'rejected', 'body-start', 'body-rejected', 'body', 'fetch-fulfilled'].includes(event.kind)), false)
        assert.equal(initial.closed, false)
        initial.release()
        await poll(() => events(2).some(event => event.kind === 'body' && event.exactBody && !event.afterPagehide))
        await poll(() => initial.closed && initial.terminal === 'finished')
        state = (await stateReceipt(2, 'body')).state
        assert.equal(state.online, true)
        assert.equal(state.failures, 0)
        assert.equal(state.requestActive, false)
      }
      else {
        await poll(() => terminal(2).length === 1)
        const outcome = terminal(2)[0]
        assert.equal(outcome.kind, cell.mode === 'owned-body' ? 'body-rejected' : 'rejected')
        assert.equal(outcome.afterPagehide, false)
        const proof = events(2).filter(event => event.kind === 'abort')
        assert.equal(proof.length, 1)
        assert.equal(proof[0].trusted, true)
        assert.equal(proof[0].exactTarget, true)
        if (cell.mode === 'outside-call') {
          assert.equal(proof[0].insideAbortCall, false)
          assert.equal(outcome.aborted, false)
          assert.equal(outcome.reasonMatches || outcome.errorName === 'AbortError', true)
          state = (await stateReceipt(2, cell.mode === 'owned-body' ? 'body-rejected' : 'fetch-rejected')).state
          assert.equal(state.online, false)
          assert.equal(state.failures, 1)
        }
        else {
          assert.equal(proof[0].insideAbortCall, true)
          assert.equal(outcome.aborted, true)
          state = (await stateReceipt(2, cell.mode === 'owned-body' ? 'body-rejected' : 'fetch-rejected')).state
          assert.equal(state.online, true)
          assert.equal(state.failures, 0)
        }
        if (!initial.headersDelivered) {
          const receipt = await stateReceipt(2, cell.mode === 'owned-body' ? 'body-rejected' : 'fetch-rejected')
          assert.equal(initial.bodyReleased, false)
          initialHeaderAdvance = {
            document: receipt.document, read: receipt.read, sourceSequence: receipt.journal.at(-1)?.sequence ?? null,
            closedBefore: initial.closed, headersDeliveredBefore: initial.headersDelivered, bodyReleasedBefore: initial.bodyReleased,
          }
          await stage('advance original headers only after the native terminal; body stays held')
          initial.releaseHeaders()
        }
        await poll(() => initial.closed && initial.terminal === 'cancelled' && !initial.bodyReleased)
        initialClosureBeforeRelease = true
        initialClosureBoundary = { closed: initial.closed, terminal: initial.terminal, headersDelivered: initial.headersDelivered, bodyReleased: initial.bodyReleased }
        assert.equal(events(2).some(event => event.kind === 'body'), false)
      }
      if (next) {
        await stage('next real periodic keepalive read before destination commit')
        await poll(() => next.ready)
        assert.equal(next.status, 200)
        if (cell.mode === 'pre-pagehide-original-body') {
          next.releaseHeaders()
          await poll(() => events(3).some(event => event.kind === 'body-start'))
          assert.equal(next.bodyReleased, false)
          assert.equal(terminal(3).length, 0)
        }
        else {
          next.release()
          await poll(() => events(3).some(event => event.kind === 'body' && event.exactBody && !event.aborted && !event.afterPagehide))
          await poll(() => next.closed && next.terminal === 'finished')
          state = (await stateReceipt(3, 'body')).state
          assert.equal(state.online, true)
          assert.equal(state.failures, 0)
        }
      }
      if (cell.mode !== 'proof-without-terminal') {
      assert.equal(destination.closed, false)
      assert.equal(destination.terminal, 'pending')
      assert.equal(received.some(event => event.document === documentId && event.kind === 'pagehide'), false)
      // Acquire a synchronous old-document journal receipt BEFORE commit;
      // page.evaluate is intentionally absent while Page.goto is outstanding.
      const lastRead = next ? 3 : 2
      const lastTrigger = cell.mode === 'pre-pagehide-original-body' ? 'fetch-fulfilled' : next || cell.mode === 'proof-without-terminal' ? 'body'
        : cell.mode === 'owned-body' || cell.mode === 'body-failure-first' ? 'body-rejected' : 'fetch-rejected'
      checkpoint = await stateReceipt(lastRead, lastTrigger)
      destination.release()
      if (cell.mode === 'pre-pagehide-original-body') {
        const commitAndJournal = observed(async () => {
          const arrival = await navigation
          assert.equal(arrival.kind, 'committed')
          navigationValidated = true
          await harvest()
          assert.equal(cleanupStartedAt, undefined)
          assert.equal(next.bodyReleased, false)
          const receipt = { departingDocument: documentId, currentDocument: checkpoint.document,
            capturedAtMs: performance.now(), beforeCleanup: cleanupStartedAt === undefined,
            bodyHeld: !next.bodyReleased, journal: checkpoint.journal, native: checkpoint.native }
          await persistEvidence(arm + '.post-commit-journal.json', receipt)
          assert.equal(cleanupStartedAt, undefined)
          assert.equal(next.bodyReleased, false)
          postCommitReceipt = { departingDocument: documentId, currentDocument: checkpoint.document,
            capturedAtMs: receipt.capturedAtMs, persistedAtMs: performance.now(), beforeCleanup: cleanupStartedAt === undefined,
            departingEvents: checkpoint.journal.filter(item => item.document === documentId).length }
          journalAcknowledged = true
        }, 'destination commit and persisted journal').catch(error => {
          remember('destination commit and persisted journal', error)
          throw error
        })
        const pauseObservation = observed(() => debuggerAction, 'first pagehide original promise observation').catch(error => {
          remember('first pagehide original promise observation', error)
          throw error
        })
        const outcomes = await Promise.allSettled([commitAndJournal, pauseObservation])
        const errors = outcomes.filter(item => item.status === 'rejected').map(item => item.reason)
        if (errors.length)
          throw errors[0]
        await expect(page.getByRole('heading', { name: 'Destination', exact: true })).toBeVisible()
        await poll(() => destination.closed && destination.terminal === 'finished')
        assert.equal(originalPromiseBoundary?.beforeFirstListenerBody, true)
        assert.equal(originalPromiseBoundary.promiseState, 'rejected')
        assert.equal(originalPromiseBoundary.resultSubtype, 'error')
        assert.equal(originalPromiseBoundary.resultClassName, 'TypeError')
        assert.equal(originalPromiseBoundary.originalBodyHeld, true)
        await poll(() => terminal(3).length === 1)
        assert.equal(terminal(3)[0].kind, 'body-rejected')
        assert.equal(terminal(3)[0].errorName, 'TypeError')
        assert.equal(terminal(3)[0].aborted, false)
        assert.equal(events(3).some(event => event.kind === 'abort'), false)
      }
      else {
        const arrival = await observed(() => navigation, 'destination commit')
        assert.equal(arrival.kind, 'committed')
        navigationValidated = true
        await expect(page.getByRole('heading', { name: 'Destination', exact: true })).toBeVisible()
        await poll(() => destination.closed && destination.terminal === 'finished')
      }
      }

    }
    await harvest()
    if (cell.mode === 'proof-without-terminal') {
      assert.equal(checkpoint.document, documentId)
      assert.equal(checkpoint.journal.some(event => event.document === documentId && event.kind === 'pagehide'), false)
    }
    journalAcknowledged = true
  }
  catch (error) { remember(phase, error) }
  finally {
    cleanupStartedAt = performance.now()
    // All failed observations remain red; closing one owner never gates another.
    waiterPendingAtCleanup = Boolean(navigation && !navigationOutcome)
    outage?.restore()
    // Start every owner's retirement without depending on a CDP reply.
    // The observation bound records failures; it never cancels an SDK command.
    for (const item of gates) item.response.release()
    const closures = [
      ...(cdpOwner ? [['CDP-close', () => cdpOwner.release()]] : []),
      ...(pageOwner ? [['page', () => pageOwner.release()]] : []),
      ...(gateOwner ? [['gate', () => gateOwner.release()]] : []),
      ['backend', () => {
        const closed = new Promise((resolve, reject) => backend.close(error => error ? reject(error) : resolve()))
        backend.closeAllConnections()
        return closed
      }],
    ]
    await Promise.all(closures.map(async ([actor, close]) => {
      try { await observed(close, actor + ' close') }
      catch (error) { cleanupFailure(actor, error) }
    }))
    if (context) {
      try { await observed(() => context.tracing.stop({ path: output + '/' + arm + '.zip' }), 'trace close') }
      catch (error) { cleanupFailure('trace', error) }
    }
    if (contextOwner) {
      try { await observed(() => contextOwner.release(), 'context close') }
      catch (error) { cleanupFailure('context', error) }
    }
    if (debuggerAction) {
      try { await observed(() => debuggerAction, 'debugger observer drain') }
      catch (error) { remember('debugger observer drain', error) }
    }
    try {
      await observed(() => Promise.allSettled([...cdpTasks]), 'CDP command drain')
      if (cdpTasks.size)
        throw new Error('CDP command closure remains unknown')
    }
    catch (error) { cleanupFailure('CDP-command-drain', error) }
    if (navigation) {
      try {
        const outcome = await observed(() => navigation, 'automation navigation waiter retirement')
        navigationRetirement = { waiterPendingAtCleanup, pageCloseCompleted, ...outcome, matchesOwnedSdkPrototype: isClosedTargetError(rawNavigationError, closedPrototype), cancellationEvidence: false }
        if (cell.mode === 'proof-without-terminal') {
          assert.equal(outcome.kind, 'rejected')
          assert.notEqual(outcome.error?.name, 'TimeoutError')
          if (waiterPendingAtCleanup) {
            assert.equal(pageCloseCompleted, true)
            assert.equal(outcome.observedAfterPageCloseStarted, true)
            assert.equal(isClosedTargetError(rawNavigationError, closedPrototype), true)
          }
        }
      }
      catch (error) { cleanupFailure('automation-navigation-waiter', error) }
    }
    try { await observed(() => Promise.all([...dialogActions]), 'native dialog action drain') }
    catch (error) { cleanupFailure('native-dialog-drain', error) }
    try { await observed(() => Promise.all([...writes]), 'receipt drain') }
    catch (error) { cleanupFailure('receipt-drain', error) }
  }
  if (cell.mode === 'proof-without-terminal' && destination?.received)
    remember('final pre-stop admission audit', new Error('Cancelled pre-stop navigation reached the destination gate'))
  try { assertArchiveObserver() } catch (error) { remember('archived observer failure audit', error) }
  finalAudit.unknown = incompleteAvailabilityReads(received)
  for (const event of received) {
    if (event.kind === 'body' && !event.exactBody)
      finalAudit.unexpected.push(event)
    const failedStatus = event.kind === 'headers' && event.status !== 200
    const rejected = ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted
    if (!failedStatus && !rejected)
      continue
    const selected = event.document === documentId && event.read === 2
    const expected = selected && ((cell.mode.endsWith('failure-first') && rejected && event.errorName === 'TypeError')
      || (cell.mode === 'outside-call' && rejected)
      || (cell.mode === 'unread-503' && failedStatus && event.status === 503))
    const nativeBoundaryFailure = cell.mode === 'pre-pagehide-original-body'
      && event.document === documentId && event.read === 3 && event.kind === 'body-rejected'
      && event.errorName === 'TypeError' && !event.aborted
      && originalPromiseBoundary?.promiseState === 'rejected'
      && originalPromiseBoundary.resultClassName === 'TypeError'
      && originalPromiseBoundary.beforeFirstListenerBody === true
    if (!expected && !nativeBoundaryFailure)
      finalAudit.unexpected.push(event)
  }
  const ownCleanup = cleanupFailures.filter(event => event.arm === arm)
  const observationComplete = hostObserverFailures === 0 && !nativeEvents.some(event => event.kind === 'observer-failed') && journalAcknowledged && navigationValidated && !failures.length && !pageErrors.length
    && !ownCleanup.length && !finalAudit.unknown.length && !finalAudit.unexpected.length
  const row = { arm, control, nativeReaderCompositionBinding, diagnosticOnly: true, productQualified: false, observationComplete, phase,
    pauseReceipts, postCommitReceipt, cleanupStartedAt, cdpAdmission, cdpCloseObserved, cdpTaskRecords, cdpCommandsDrained: cdpTasks.size === 0, originalPromiseBoundary, documentId, state, pendingProof, stateReceipts, dialogs, nativeNavigationCheckpoint, navigationRetirement, activatedViaClick, retainedAfterDialog, destinationAdmission: cell.mode === 'proof-without-terminal' && navigationValidated && !destination?.received ? 'cancelled-before-request' : null, outageEvidence, initialHeaderAdvance, initialClosureBoundary, initialClosureBeforeRelease, navigationValidated, journalAcknowledged,
    allPageErrorsStrict: pageErrors.length === 0, hostObserverFailures, pageErrors, failures, reads: received, nativeEvents, finalAudit,
    gates: gates.map(({ label, response }) => ({ label, received: response.received, ready: response.ready, status: response.status ?? null, terminal: response.terminal, closed: response.closed, headersDelivered: response.headersDelivered, bodyReleased: response.bodyReleased })), cleanupFailures: ownCleanup }
  rows.push(row)
  await write(arm + '.json', row)
  console.log(JSON.stringify({ control, arm, observationComplete, pageErrors: pageErrors.length, cleanupFailures: ownCleanup.length }))
}

for (const engine of new Set(planned.map(cell => cell.engine))) {
  let browser, browserOwner
  try {
    browserOwner = ownSdkResource(() => ({ chromium, webkit })[engine].launch({ timeout: 7000 }), value => value.close())
    browser = await observed(() => browserOwner.result, engine + ' launch')
    const closedReference = await acquireClosedTargetReference(browser, errors, observed)
    await write(engine + '-closed-target-sdk.json', closedReference.receipt)
    for (const cell of planned.filter(item => item.engine === engine)) {
      await observe(browser, cell, closedReference.prototype)
      if (!rows.at(-1).observationComplete || cleanupFailures.length)
        break
    }
  }
  catch (error) { engineFailures.push({ engine, error: errorRecord(error) }) }
  finally {
    if (browserOwner) {
      try { await observed(() => browserOwner.release(), engine + ' browser close') }
      catch (error) { cleanupFailures.push({ actor: engine + '-browser', error: errorRecord(error) }) }
    }
  }
  if (cleanupFailures.length || engineFailures.length || rows.some(row => !row.observationComplete))
    break
}
const allCellsObserved = rows.length === planned.length && rows.every(row => row.observationComplete)
const result = { control, diagnosticOnly: true, productQualified: false, retainedPollingQualified: false, bfCacheQualified: false,
  genuineFailureHandlingQualified: false, nativeReaderCompositionBinding, sourceHashes, acquiredPositiveEvidence, acquiredNegativeEvidence, fullEightCellRunComplete: false, planned, allCellsObserved,
  allPageErrorsStrict: rows.every(row => row.allPageErrorsStrict), rows, cleanupFailures, engineFailures }
await write('result.json', result)
console.log(JSON.stringify({ ...result, rows: rows.length, cleanupFailures: cleanupFailures.length, engineFailures: engineFailures.length }))
if (!allCellsObserved || cleanupFailures.length || engineFailures.length)
  process.exitCode = 1
