import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { stripTypeScriptTypes } from 'node:module'
import { devices, expect, webkit } from '@playwright/test'

const output = 'test-results/reader-native'
await mkdir(output, { recursive: true })
const control = 'webkit-next-read-after-initial-gate-retirement'
const paths = {
  gate: 'tests/e2e/native-response-gate.ts',
  probe: 'tests/e2e/availability-probe.ts',
  transport: 'apps/web/src/availability-read.ts',
  shell: 'apps/web/src/BeaconApp.vue',
}
const sources = Object.fromEntries(await Promise.all(Object.entries(paths).map(async ([key, path]) => [key, await readFile(path, 'utf8')])))
const sourceHashes = Object.fromEntries(Object.entries(sources).map(([key, text]) => [key, createHash('sha256').update(text).digest('hex')]))
const loadTypeScript = text => import('data:text/javascript;base64,' + Buffer.from(stripTypeScriptTypes(text)).toString('base64'))
const { nativeResponseGate } = await loadTypeScript(sources.gate)
const { installAvailabilityProbe } = await loadTypeScript(sources.probe)
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
  window.diagnosticState = () => ({ online: state.installationOnline, failures: availabilityFailures, suspended: availabilitySuspended });
  void refreshAvailability();
};`
const rows = []
const cleanupFailures = []
const planned = ['initial-held', 'initial-retired']
const errorRecord = error => ({ name: error?.name ?? 'Error', message: String(error?.message ?? error), stack: error?.stack })
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

function auditReadJournal(reads, selectedDocument, selectedRead, selectedOutcome) {
  const starts = new Map()
  for (const event of reads) {
    if (event.kind === 'start') {
      const key = event.document + ':' + event.read
      starts.set(key, [...(starts.get(key) ?? []), event])
    }
  }
  const unknownReads = []
  const runtimeFailures = []
  for (const [key, begun] of starts) {
    const events = reads.filter(event => event.document === begun[0].document && event.read === begun[0].read)
    const terminals = events.filter(event => ['body', 'body-rejected', 'rejected'].includes(event.kind))
    if (begun.length !== 1 || terminals.length !== 1) {
      unknownReads.push({ key, starts: begun.length, terminals: terminals.length })
      continue
    }
    const terminal = terminals[0]
    const failedStatus = events.find(event => event.kind === 'headers' && event.status !== 200)
    const ownedAbort = terminal.kind === 'rejected' && terminal.aborted
      && events.some(event => event.kind === 'abort' && event.trusted && event.exactTarget && event.insideAbortCall)
    const preciseSuccess = terminal.kind === 'body' && terminal.exactBody && !terminal.aborted && !terminal.afterPagehide
    if (failedStatus || (!ownedAbort && !preciseSuccess)) {
      const selectedFailure = !failedStatus && terminal.document === selectedDocument && terminal.read === selectedRead
        && selectedOutcome === 'unowned-rejection-without-http' && terminal.kind === 'rejected'
        && !terminal.aborted && !terminal.afterPagehide
      runtimeFailures.push({ key, kind: terminal.kind, status: failedStatus?.status ?? null, selectedFailure })
    }
  }
  return { startedReads: starts.size, unknownReads, runtimeFailures, unexpectedFailures: runtimeFailures.filter(failure => !failure.selectedFailure) }
}

// Adds metadata only. The actual transport, poll and probe remain source-bound.
function nativeDiagnostics() {
  let nextRead = 0
  const reads = new WeakMap()
  const journal = []
  const record = (kind, read, extra = {}) => {
    const event = {
      kind, read, sequence: journal.length + 1,
      readyState: document.readyState, hidden: document.hidden, windowClosed: window.closed,
      ...extra,
    }
    journal.push(event)
    sessionStorage.setItem('cairn-gate-native-journal', JSON.stringify(journal))
    void window.nativeDiagnosticReceipt(event)
  }
  const open = XMLHttpRequest.prototype.open
  const send = XMLHttpRequest.prototype.send
  XMLHttpRequest.prototype.open = function (...args) {
    if (args[0] === 'GET' && String(args[1]) === '/api/installations') {
      const read = ++nextRead
      reads.set(this, read)
      for (const kind of ['error', 'timeout', 'abort', 'load']) {
        this.addEventListener(kind, event => record('native-' + kind, read, {
          trusted: event.isTrusted, exactTarget: event.target === this && event.currentTarget === this,
          xhrReadyState: this.readyState, status: this.status,
        }), { capture: true })
      }
    }
    return open.apply(this, args)
  }
  XMLHttpRequest.prototype.send = function (...args) {
    const read = reads.get(this)
    if (read)
      record('send-enter', read)
    try { return send.apply(this, args) }
    catch (error) {
      if (read)
        record('send-throw', read, { name: error.name, message: error.message })
      throw error
    }
    finally {
      if (read)
        record('send-return', read)
    }
  }
}

async function observe(browser, arm) {
  const received = []
  const nativeEvents = []
  const pageErrors = []
  const failures = []
  const receiptWrites = new Set()
  const gates = []
  let phase = 'backend setup'
  let context, page, gate, navigation, documentId, firstRead, nextRead
  let navigationValidated = false
  let journalAcknowledged = false
  let initialRetiredBeforeNextStart = false
  let retirementBoundary
  let nextOutcome = 'unknown'
  let initial, destination, next
  const ownedFailure = (actor, error) => cleanupFailures.push({ arm, actor, error: errorRecord(error) })
  const remember = (stage, error) => failures.push({ stage, error: errorRecord(error) })
  const stage = async label => {
    phase = label
    console.log(JSON.stringify({ control, arm, phase }))
    await write('active.json', { control, arm, phase, received, nativeEvents, pageErrors, failures, complete: false })
  }
  const saveReceipt = (journal, suffix, event) => {
    journal.push(event)
    const promise = appendFile(output + '/' + arm + '.' + suffix + '.jsonl', JSON.stringify(event) + '\n')
    receiptWrites.add(promise)
    return promise.catch(error => { ownedFailure('receipt-write', error); throw error }).finally(() => receiptWrites.delete(promise))
  }
  const evaluate = (fn, argument) => observed(() => page.evaluate(fn, argument), 'document evaluation: ' + phase)
  const events = read => received.filter(event => event.document === documentId && event.read === read)
  const poll = operation => expect.poll(() => { gate.check(); return operation() }, { timeout: 7000 }).toBe(true)
  const backend = createServer((request, response) => {
    response.setHeader('cache-control', 'no-store')
    if (request.url === '/api/installations') {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body))
    }
    else if (request.url === '/live') {
      response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Live</h1><script src="/client.js"></script>')
    }
    else if (request.url === '/client.js') {
      response.writeHead(200, { 'content-type': 'application/javascript' }).end(bootstrap)
    }
    else {
      response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Destination</h1>')
    }
  })
  try {
    await stage('backend setup')
    backend.listen(0, '127.0.0.1')
    await observed(() => once(backend, 'listening'), 'backend listening')
    const origin = 'http://127.0.0.1:' + backend.address().port
    gate = await observed(() => nativeResponseGate(origin), 'gate setup')
    const { defaultBrowserType: _, ...device } = devices['Desktop Chrome']
    context = await observed(() => browser.newContext({ ...device, isMobile: true, hasTouch: true, proxy: { server: gate.server } }), 'context setup')
    context.setDefaultTimeout(7000)
    context.setDefaultNavigationTimeout(30000)
    await observed(() => context.tracing.start({ screenshots: false, snapshots: true, sources: true }), 'trace setup')
    page = await observed(() => context.newPage(), 'page setup')
    page.on('pageerror', error => pageErrors.push(errorRecord(error)))
    await observed(() => installAvailabilityProbe(page, event => saveReceipt(received, 'reads', event)), 'actual probe setup')
    await observed(() => page.exposeFunction('nativeDiagnosticReceipt', event => saveReceipt(nativeEvents, 'native', event)), 'native metadata binding')
    await observed(() => page.addInitScript(nativeDiagnostics), 'native metadata setup')
    await stage('actual poll initial success after native pageshow')
    await observed(() => page.clock.install(), 'same primary clock install')
    await page.goto(origin + '/live')
    await poll(() => received.some(event => event.kind === 'pageshow' && event.trusted))
    documentId = await evaluate(() => window.cairnAvailabilityDocument)
    await evaluate(() => window.startDiagnosticPoll())
    await poll(() => events(1).some(event => event.kind === 'body' && event.exactBody))
    await observed(() => page.clock.pauseAt(new Date(Date.now() + 1000)), 'same primary clock pause')
    const earlierReads = received.filter(event => event.document === documentId && event.kind === 'start').length
    initial = gate.hold('availability'); gates.push({ label: 'initial', response: initial })
    destination = gate.hold('agents'); gates.push({ label: 'destination', response: destination })
    await stage('actual periodic read held at real HTTP gate')
    await observed(() => page.clock.runFor(3100), 'same primary clock progression')
    await poll(() => initial.ready)
    assert.equal(initial.status, 200)
    await poll(() => received.filter(event => event.document === documentId && event.kind === 'start').length === earlierReads + 1)
    firstRead = received.filter(event => event.document === documentId && event.kind === 'start').at(-1).read
    await observed(() => page.clock.resume(), 'same primary real-time clock resume')
    await stage('native Page.goto destination held')
    navigation = page.goto(origin + '/installations/fixture/agents').then(() => ({ kind: 'committed' }), error => ({ kind: 'rejected', error: errorRecord(error) }))
    await poll(() => destination.ready)
    assert.equal(destination.status, 200)
    await poll(() => events(firstRead).some(event => event.kind === 'abort' && event.trusted && event.exactTarget && event.insideAbortCall && !event.afterPagehide))
    await poll(() => events(firstRead).some(event => event.kind === 'rejected' && event.aborted))
    assert.equal(events(firstRead).some(event => event.kind === 'body'), false)
    assert.equal(received.some(event => event.document === documentId && event.kind === 'pagehide'), false)
    next = gate.hold('availability'); gates.push({ label: 'next', response: next })
    if (arm === 'initial-retired') {
      await stage('only changed variable: release and acknowledge initial HTTP closure')
      initial.release()
      await poll(() => initial.closed && ['cancelled', 'finished'].includes(initial.terminal))
      // Exposed-function messages can lag the synchronous browser journal. Only
      // a browser checkpoint acquired AFTER HTTP closure can prove the ordering.
      const checkpoint = await evaluate(() => ({
        document: window.cairnAvailabilityDocument,
        reads: JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]'),
      }))
      assert.equal(checkpoint.document, documentId)
      initialRetiredBeforeNextStart = !checkpoint.reads.some(event => event.document === documentId && event.kind === 'start' && event.read > firstRead)
      retirementBoundary = { document: checkpoint.document, lastSequence: checkpoint.reads.at(-1)?.sequence ?? null, beforeNextStart: initialRetiredBeforeNextStart }
      assert.equal(initialRetiredBeforeNextStart, true)
    }
    await stage('next ordinary periodic read while destination still held')
    await poll(() => received.some(event => event.document === documentId && event.kind === 'start' && event.read > firstRead))
    nextRead = received.find(event => event.document === documentId && event.kind === 'start' && event.read > firstRead).read
    await poll(() => next.ready || events(nextRead).some(event => ['rejected', 'body-rejected'].includes(event.kind)))
    if (next.ready) {
      assert.equal(next.status, 200)
      next.release()
      await poll(() => events(nextRead).some(event => event.kind === 'body' && event.exactBody)
        || events(nextRead).some(event => ['rejected', 'body-rejected'].includes(event.kind)))
    }
    const terminal = events(nextRead).filter(event => ['body', 'body-rejected', 'rejected'].includes(event.kind))
    assert.equal(terminal.length, 1)
    assert.equal(terminal[0].afterPagehide, false)
    nextOutcome = terminal[0].kind === 'body' && terminal[0].exactBody && next.ready
      ? 'exact-200-body' : terminal[0].kind === 'rejected' && !terminal[0].aborted && !next.received
        ? 'unowned-rejection-without-http' : 'other-failure'
    // Preserve the failure as a red product result even when it calibrates the
    // diagnostic baseline. Only owned complete closure permits the second arm.
    assert.notEqual(nextOutcome, 'other-failure')
    assert.equal(destination.terminal, 'pending')
    assert.equal(destination.closed, false)
    assert.equal(received.some(event => event.document === documentId && event.kind === 'pagehide'), false)
    await stage('release held HTTP and acknowledge destination before final journal')
    initial.release(); next.release(); destination.release()
    const arrival = await observed(() => navigation, 'navigation after release')
    assert.equal(arrival.kind, 'committed')
    navigationValidated = true
    await expect(page.getByRole('heading', { name: 'Destination', exact: true })).toBeVisible()
    for (const response of [initial, next, destination]) {
      // A gate with no received HTTP request is explicitly unobserved, not closed.
      if (response.ready)
        await poll(() => response.closed && ['cancelled', 'finished'].includes(response.terminal))
    }
    const journal = await evaluate(() => JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]'))
    for (const event of journal) {
      if (!received.some(entry => entry.document === event.document && entry.sequence === event.sequence))
        received.push(event)
    }
    journalAcknowledged = true
  }
  catch (error) { remember(phase, error) }
  finally {
    for (const item of gates) item.response.release()
    // Each owner starts independently of another owner's stalled closure.
    const closures = [
      ...(page ? [['page', () => page.close()]] : []),
      ...(gate ? [['gate', () => gate.close()]] : []),
      ['backend', () => {
        const closed = new Promise((resolve, reject) => backend.close(error => error ? reject(error) : resolve()))
        backend.closeAllConnections()
        return closed
      }],
    ]
    await Promise.all(closures.map(async ([actor, close]) => {
      try { await observed(close, actor + ' close') }
      catch (error) { ownedFailure(actor, error) }
    }))
    if (context) {
      try { await observed(() => context.tracing.stop({ path: output + '/' + arm + '.zip' }), 'trace close') }
      catch (error) { ownedFailure('trace', error) }
      try { await observed(() => context.close(), 'context close') }
      catch (error) { ownedFailure('context', error) }
    }
    if (navigation) {
      try { await observed(() => navigation, 'navigation settlement after owned closures') }
      catch (error) { ownedFailure('navigation', error) }
    }
    try { await observed(() => Promise.all([...receiptWrites]), 'receipt drain after context closure') }
    catch (error) { ownedFailure('receipt-drain', error) }
  }
  // Absence is meaningful only after the page/context/proxy owners have closed:
  // a request can arrive after a binding receipt but before network teardown.
  if (nextOutcome === 'unowned-rejection-without-http' && next.received) {
    nextOutcome = 'unowned-rejection-after-http'
    remember('final HTTP ingress audit', new Error('Replacement HTTP request arrived before owned closure'))
  }
  const readAudit = auditReadJournal(received, documentId, nextRead, nextOutcome)
  const ownFailures = cleanupFailures.filter(entry => entry.arm === arm)
  const observationComplete = !failures.length && !ownFailures.length && !pageErrors.length && navigationValidated && journalAcknowledged
    && nextOutcome !== 'unknown' && readAudit.unknownReads.length === 0 && readAudit.unexpectedFailures.length === 0
  const row = {
    arm, control, diagnosticOnly: true, productQualified: false,
    observationComplete, nextOutcome, firstRead, nextRead, documentId,
    initialRetiredBeforeNextStart, retirementBoundary, navigationValidated, journalAcknowledged,
    allPageErrorsStrict: pageErrors.length === 0, pageErrors, failures,
    reads: received, nativeEvents, readAudit,
    gates: gates.map(({ label, response }) => ({ label, received: response.received, ready: response.ready, status: response.status ?? null, terminal: response.terminal, closed: response.closed })),
    cleanupFailures: ownFailures,
  }
  rows.push(row)
  await write(arm + '.json', row)
  console.log(JSON.stringify({ control, arm, observationComplete, nextOutcome, pageErrors: pageErrors.length, cleanupFailures: ownFailures.length }))
}

let browser
const engineFailures = []
try {
  browser = await observed(() => webkit.launch(), 'webkit launch')
  for (const arm of planned) {
    await observe(browser, arm)
    const row = rows.at(-1)
    if (!row.observationComplete || cleanupFailures.length)
      break
    if (arm === 'initial-held' && row.nextOutcome !== 'unowned-rejection-without-http')
      break // Baseline did not reproduce: do not claim a gate-retirement cause.
  }
}
catch (error) { engineFailures.push(errorRecord(error)) }
finally {
  if (browser) {
    try { await observed(() => browser.close(), 'browser close') }
    catch (error) { cleanupFailures.push({ actor: 'browser', error: errorRecord(error) }) }
  }
}
const allCellsObserved = rows.length === planned.length && rows.every(row => row.observationComplete)
const baselineMatched = rows[0]?.observationComplete && rows[0]?.nextOutcome === 'unowned-rejection-without-http'
const contrastObserved = allCellsObserved && baselineMatched && rows[1].initialRetiredBeforeNextStart
const result = {
  control, diagnosticOnly: true, productQualified: false, bfCacheQualified: false,
  retainedPollingQualified: false, genuineFailureHandlingQualified: false,
  sourceHashes, planned, allCellsObserved, baselineMatched: Boolean(baselineMatched),
  contrastObserved: Boolean(contrastObserved),
  earlyRetirementAdmitsExactBody: contrastObserved ? rows[1].nextOutcome === 'exact-200-body' : null,
  allPageErrorsStrict: rows.every(row => row.allPageErrorsStrict),
  rows, cleanupFailures, engineFailures,
}
await write('result.json', result)
console.log(JSON.stringify({ ...result, rows: rows.length, cleanupFailures: cleanupFailures.length, engineFailures: engineFailures.length }))
// A reproduced genuine rejection or any pageerror remains red. This diagnostic
// never launders a failing baseline into a green product qualification.
if (!allCellsObserved || !contrastObserved || rows.some(row => row.nextOutcome !== 'exact-200-body' || !row.allPageErrorsStrict)
  || cleanupFailures.length || engineFailures.length)
  process.exitCode = 1
