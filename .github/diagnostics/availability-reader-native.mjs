import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { stripTypeScriptTypes } from 'node:module'
import { chromium, devices, expect, webkit } from '@playwright/test'

const output = 'test-results/reader-native'
await mkdir(output, { recursive: true })
const control = 'owned-keepalive-fetch-original-terminals'
const paths = {
  gate: 'tests/e2e/native-response-gate.ts',
  probe: 'tests/e2e/availability-probe.ts',
  transport: 'apps/web/src/availability-read.ts',
  shell: 'apps/web/src/BeaconApp.vue',
}
const sources = Object.fromEntries(await Promise.all(Object.entries(paths).map(async ([key, path]) => [key, await readFile(path, 'utf8')])))
const sourceHashes = Object.fromEntries(Object.entries(sources).map(([key, value]) => [key, createHash('sha256').update(value).digest('hex')]))
const loadTypeScript = text => import('data:text/javascript;base64,' + Buffer.from(stripTypeScriptTypes(text)).toString('base64'))
const { nativeResponseGate } = await loadTypeScript(sources.gate)
const { installAvailabilityProbe, incompleteAvailabilityReads } = await loadTypeScript(sources.probe)
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
  { engine: 'chromium', mode: 'owned-fetch' },
  { engine: 'webkit', mode: 'owned-fetch' },
  { engine: 'webkit', mode: 'owned-body' },
  { engine: 'webkit', mode: 'fetch-failure-first' },
  { engine: 'webkit', mode: 'body-failure-first' },
  { engine: 'webkit', mode: 'unread-503' },
  { engine: 'webkit', mode: 'outside-call' },
  { engine: 'webkit', mode: 'proof-without-terminal' },
]
const rows = []
const cleanupFailures = []
const engineFailures = []
const errorRecord = error => ({ name: error?.name ?? typeof error, message: String(error?.message ?? error) })
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
  window.addEventListener('pagehide', () => {
    if (channel) { channel.port1.close(); channel.port2.close(); channel = undefined }
    snapshots.length = 0
  })
  const record = (kind, extra = {}) => {
    const journal = JSON.parse(sessionStorage.getItem('cairn-native-fetch-journal') || '[]')
    journal.push({ kind, sequence: journal.length + 1, ...extra })
    sessionStorage.setItem('cairn-native-fetch-journal', JSON.stringify(journal))
  }
  const signals = new WeakMap()
  AbortController.prototype.abort = function (reason) {
    const target = signals.get(this.signal)
    if (mode === 'outside-call' && target === 2) {
      queueMicrotask(() => abort.call(this, reason))
      record('deferred-owned-call-return', { read: target })
      return
    }
    return abort.call(this, reason)
  }
  window.fetch = function (input, init) {
    if (String(input) !== '/api/installations')
      return fetch.call(this, input, init)
    const current = ++read
    const signal = init?.signal
    if (signal) {
      signals.set(signal, current)
      signal.addEventListener('abort', event => {
        record('private-native-signal', {
          read: current, trusted: event.isTrusted,
          exactTarget: event.target === signal && event.currentTarget === signal && event instanceof Event,
          aborted: signal.aborted,
        })
        snapshot('signal', current)
      })
    }
    let request = init
    if (mode === 'proof-without-terminal' && current === 2) {
      const wire = new AbortController()
      record('negative-wire-substitution', { read: current, distinctSignal: wire.signal !== signal })
      request = { ...init, signal: wire.signal }
    }
    const original = fetch.call(this, input, request)
    void original.then(response => {
      responses.set(response, current)
      snapshot('fetch-fulfilled', current)
    }, () => snapshot('fetch-rejected', current))
    return original
  }
  Response.prototype.json = function () {
    const original = json.call(this)
    const current = responses.get(this)
    if (current)
      void original.then(() => snapshot('body', current), () => snapshot('body-rejected', current))
    return original
  }
}

async function observe(browser, cell) {
  const arm = cell.engine + '-' + cell.mode
  const received = []
  const stateReceipts = []
  const pageErrors = []
  const failures = []
  const writes = new Set()
  const gates = []
  let phase = 'setup'
  let context, page, gate, navigation, documentId, initial, destination, next
  let checkpoint, nativeEvents = [], initialClosureBeforeRelease = false, journalAcknowledged = false, navigationValidated = false
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
    gate = await observed(() => nativeResponseGate(origin), 'gate setup')
    const { defaultBrowserType: _, ...device } = devices['Desktop Chrome']
    context = await observed(() => browser.newContext({ ...device, isMobile: true, hasTouch: true, proxy: { server: gate.server } }), 'context setup')
    context.setDefaultTimeout(7000)
    context.setDefaultNavigationTimeout(30000)
    await observed(() => context.tracing.start({ screenshots: false, snapshots: true, sources: true }), 'trace setup')
    page = await observed(() => context.newPage(), 'page setup')
    page.on('pageerror', error => pageErrors.push(errorRecord(error)))
    // One init script fixes adapter ordering before the actual product captures
    // its producer functions. The probe itself is the unmodified source callback.
    const initializers = []
    await installAvailabilityProbe({
      exposeFunction: (...args) => page.exposeFunction(...args),
      addInitScript: callback => { initializers.push(callback.toString()) },
    }, record)
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
    assert.equal(initializers.length, 1)
    await page.addInitScript(`(${nativeDiagnostics.toString()})(${JSON.stringify(cell.mode)});(${initializers[0]})();`)
    await stage('same actual poll and clock seed')
    await page.clock.install()
    await page.goto(origin + '/live')
    await poll(() => received.some(event => event.kind === 'pageshow' && event.trusted))
    documentId = await evaluate(() => window.cairnAvailabilityDocument)
    await evaluate(() => window.startDiagnosticPoll())
    await poll(() => events(1).some(event => event.kind === 'body' && event.exactBody))
    await page.clock.pauseAt(new Date(Date.now() + 1000))
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
      await poll(() => initial.closed && initial.terminal === 'cancelled')
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
        initial.disconnect()
        await poll(() => terminal(2).length === 1)
        assert.equal(terminal(2)[0].kind, cell.mode === 'fetch-failure-first' ? 'rejected' : 'body-rejected')
        assert.equal(terminal(2)[0].aborted, false)
        assert.equal(terminal(2)[0].errorName, 'TypeError')
        state = (await stateReceipt(2, cell.mode === 'fetch-failure-first' ? 'fetch-rejected' : 'body-rejected')).state
        assert.equal(state.online, false)
        assert.equal(state.failures, 1)
        await poll(() => initial.closed && initial.terminal === 'cancelled')
        initialClosureBeforeRelease = true
      }
      destination = addGate('destination', gate.hold('agents'))
      if (cell.mode.startsWith('owned-'))
        next = addGate('replacement', gate.hold('availability'))
      await stage('original Page.goto with destination response held')
      navigation = page.goto(origin + '/installations/fixture/agents').then(() => ({ kind: 'committed' }), error => ({ kind: 'rejected', error: errorRecord(error) }))
      await poll(() => destination.ready)
      assert.equal(destination.status, 200)
      if (cell.mode.endsWith('failure-first')) {
        assert.equal(events(2).some(event => event.kind === 'abort'), false)
      }
      else if (cell.mode === 'proof-without-terminal') {
        await stage('owned signal alone cannot settle the original held fetch')
        await poll(() => events(2).some(event => event.kind === 'abort' && event.trusted && event.exactTarget && event.insideAbortCall))
        pendingProof = await stateReceipt(2, 'signal')
        assert.equal(pendingProof.state.requestActive, true)
        assert.equal(pendingProof.journal.some(event => event.document === documentId && event.read === 2 && ['rejected', 'body-rejected', 'body', 'fetch-fulfilled'].includes(event.kind)), false)
        assert.equal(initial.closed, false)
        initial.release()
        await poll(() => events(2).some(event => event.kind === 'body' && event.exactBody))
        await poll(() => initial.closed && initial.terminal === 'finished')
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
        await poll(() => initial.closed && initial.terminal === 'cancelled')
        initialClosureBeforeRelease = true
      }
      if (next) {
        await stage('next real periodic keepalive read before destination commit')
        await poll(() => next.ready)
        assert.equal(next.status, 200)
        next.release()
        await poll(() => events(3).some(event => event.kind === 'body' && event.exactBody && !event.aborted && !event.afterPagehide))
        await poll(() => next.closed && next.terminal === 'finished')
        state = (await stateReceipt(3, 'body')).state
        assert.equal(state.online, true)
        assert.equal(state.failures, 0)
      }
      assert.equal(destination.closed, false)
      assert.equal(destination.terminal, 'pending')
      assert.equal(received.some(event => event.document === documentId && event.kind === 'pagehide'), false)
      // Acquire a synchronous old-document journal receipt BEFORE commit;
      // page.evaluate is intentionally absent while Page.goto is outstanding.
      const lastRead = next ? 3 : 2
      const lastTrigger = next || cell.mode === 'proof-without-terminal' ? 'body'
        : cell.mode === 'owned-body' || cell.mode === 'body-failure-first' ? 'body-rejected' : 'fetch-rejected'
      checkpoint = await stateReceipt(lastRead, lastTrigger)
      destination.release()
      const arrival = await observed(() => navigation, 'destination commit')
      assert.equal(arrival.kind, 'committed')
      navigationValidated = true
      await expect(page.getByRole('heading', { name: 'Destination', exact: true })).toBeVisible()
      await poll(() => destination.closed && destination.terminal === 'finished')
    }
    await harvest()
    journalAcknowledged = true
  }
  catch (error) { remember(phase, error) }
  finally {
    // All failed observations remain red; closing one owner never gates another.
    for (const item of gates) item.response.release()
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
      catch (error) { cleanupFailure(actor, error) }
    }))
    if (context) {
      try { await observed(() => context.tracing.stop({ path: output + '/' + arm + '.zip' }), 'trace close') }
      catch (error) { cleanupFailure('trace', error) }
      try { await observed(() => context.close(), 'context close') }
      catch (error) { cleanupFailure('context', error) }
    }
    if (navigation) {
      try { await observed(() => navigation, 'navigation settlement') }
      catch (error) { cleanupFailure('navigation', error) }
    }
    try { await observed(() => Promise.all([...writes]), 'receipt drain') }
    catch (error) { cleanupFailure('receipt-drain', error) }
  }
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
    if (!expected)
      finalAudit.unexpected.push(event)
  }
  const ownCleanup = cleanupFailures.filter(event => event.arm === arm)
  const observationComplete = journalAcknowledged && navigationValidated && !failures.length && !pageErrors.length
    && !ownCleanup.length && !finalAudit.unknown.length && !finalAudit.unexpected.length
  const row = { arm, control, diagnosticOnly: true, productQualified: false, observationComplete, phase,
    documentId, state, pendingProof, stateReceipts, initialClosureBeforeRelease, navigationValidated, journalAcknowledged,
    allPageErrorsStrict: pageErrors.length === 0, pageErrors, failures, reads: received, nativeEvents, finalAudit,
    gates: gates.map(({ label, response }) => ({ label, received: response.received, ready: response.ready, status: response.status ?? null, terminal: response.terminal, closed: response.closed })), cleanupFailures: ownCleanup }
  rows.push(row)
  await write(arm + '.json', row)
  console.log(JSON.stringify({ control, arm, observationComplete, pageErrors: pageErrors.length, cleanupFailures: ownCleanup.length }))
}

for (const engine of ['chromium', 'webkit']) {
  let browser
  try {
    browser = await observed(() => ({ chromium, webkit })[engine].launch(), engine + ' launch')
    for (const cell of planned.filter(item => item.engine === engine)) {
      await observe(browser, cell)
      if (!rows.at(-1).observationComplete || cleanupFailures.length)
        break
    }
  }
  catch (error) { engineFailures.push({ engine, error: errorRecord(error) }) }
  finally {
    if (browser) {
      try { await observed(() => browser.close(), engine + ' browser close') }
      catch (error) { cleanupFailures.push({ actor: engine + '-browser', error: errorRecord(error) }) }
    }
  }
  if (cleanupFailures.length || engineFailures.length || rows.some(row => !row.observationComplete))
    break
}
const allCellsObserved = rows.length === planned.length && rows.every(row => row.observationComplete)
const result = { control, diagnosticOnly: true, productQualified: false, retainedPollingQualified: false, bfCacheQualified: false,
  genuineFailureHandlingQualified: false, sourceHashes, planned, allCellsObserved,
  allPageErrorsStrict: rows.every(row => row.allPageErrorsStrict), rows, cleanupFailures, engineFailures }
await write('result.json', result)
console.log(JSON.stringify({ ...result, rows: rows.length, cleanupFailures: cleanupFailures.length, engineFailures: engineFailures.length }))
if (!allCellsObserved || cleanupFailures.length || engineFailures.length)
  process.exitCode = 1
