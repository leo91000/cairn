import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { appendFile, mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { stripTypeScriptTypes } from 'node:module'
import { chromium, expect, webkit } from '@playwright/test'

const output = 'test-results/reader-native'
await mkdir(output, { recursive: true })
const gateBytes = await readFile('tests/e2e/native-response-gate.ts')
const gateModule = Buffer.from(stripTypeScriptTypes(gateBytes.toString())).toString('base64')
const { nativeResponseGate } = await import('data:text/javascript;base64,' + gateModule)

// This profile proves only the missing trusted-event/scope/native-reason seam.
// It does not qualify retained polling, BFCache, the application or a repair.
const control = 'beforeunload-native-scope-only'
const expectedCells = ['chromium:beforeunload-scope', 'webkit:beforeunload-scope']
const rows = []
const cleanupFailures = []
const engineFailures = []

function client() {
  const key = 'cairn-reader-mechanism'
  const capturedWindow = window
  const capturedDocument = document
  const documentId = crypto.randomUUID()
  const controller = new AbortController()
  let active = false
  let started = false
  let listening = false
  let withdrawalReason
  let eventId = 0
  const record = (kind, detail = {}) => {
    const journal = JSON.parse(sessionStorage.getItem(key) || '[]')
    const entry = { document: documentId, read: 1, kind, sequence: journal.length + 1, time: performance.now(), ...detail }
    journal.push(entry)
    sessionStorage.setItem(key, JSON.stringify(journal))
    void capturedWindow.readerReceipt?.(entry).catch(() => {})
  }
  const removeListener = () => {
    if (!listening)
      return
    capturedWindow.removeEventListener('beforeunload', beforeUnload)
    listening = false
    record('listener-removed', { listening })
  }
  const finish = () => {
    active = false
    removeListener()
    record('settled', { active, listening })
  }
  function beforeUnload(event) {
    const id = ++eventId
    // beforeunload has the legacy document target, not NavigateEvent's target.
    // Capture currentTarget synchronously; it becomes null after dispatch.
    const scope = {
      id,
      type: event.type,
      trusted: event.isTrusted,
      nativeInterface: typeof capturedWindow.BeforeUnloadEvent === 'function' && event instanceof capturedWindow.BeforeUnloadEvent,
      targetDocument: event.target === capturedDocument,
      targetWindow: event.target === capturedWindow,
      currentWindow: event.currentTarget === capturedWindow,
      documentCurrent: capturedWindow.document === capturedDocument,
      defaultViewMatches: capturedDocument.defaultView === capturedWindow,
      topLevel: capturedWindow.top === capturedWindow,
      defaultPrevented: event.defaultPrevented,
    }
    const verified = scope.type === 'beforeunload' && scope.trusted && scope.nativeInterface
      && scope.targetDocument && !scope.targetWindow && scope.currentWindow
      && scope.documentCurrent && scope.defaultViewMatches && scope.topLevel
    record('departure-intent', { ...scope, verified })
    if (!verified || !active)
      return
    // Do not use once:true: an untrusted dispatch must not consume protection.
    // This listener never prevents navigation, sets returnValue or prompts.
    removeListener()
    withdrawalReason = { kind: 'beforeunload', id, document: documentId, read: 1 }
    record('owner-withdrawal', { id, listening })
    controller.abort(withdrawalReason)
  }
  capturedWindow.readerDocument = documentId
  capturedWindow.addEventListener('pagehide', (event) => {
    record('pagehide', { persisted: event.persisted })
    removeListener()
    if (active)
      controller.abort()
  })
  controller.signal.addEventListener('abort', () => {
    record('signal-abort', {
      ownedReason: Boolean(withdrawalReason && controller.signal.reason === withdrawalReason),
      id: controller.signal.reason?.id,
    })
    removeListener()
  }, { once: true })
  capturedWindow.beginReader = () => {
    if (started)
      throw new Error('The scope control owns exactly one read')
    started = true
    active = true
    record('start')
    capturedWindow.addEventListener('beforeunload', beforeUnload)
    listening = true
    record('listener-attached', { active, listening })
    // Observe the native promise itself: no ownership race can manufacture a
    // native terminal. Destination headers remain held until proof is durable.
    void fetch('/api/installations', { credentials: 'same-origin', keepalive: true, signal: controller.signal }).then(async (response) => {
      record('native-headers', { status: response.status })
      try {
        await response.json()
        record('native-body')
      }
      catch (error) {
        record('native-body-rejected', { sameAbortReason: controller.signal.aborted && error === controller.signal.reason })
      }
      finally {
        finish()
      }
    }, (error) => {
      record('native-fetch-rejected', {
        signalAborted: controller.signal.aborted,
        typeError: error instanceof TypeError,
        abortError: typeof DOMException === 'function' && error instanceof DOMException && error.name === 'AbortError',
        sameAbortReason: controller.signal.aborted && error === controller.signal.reason,
        ownedReason: Boolean(withdrawalReason && controller.signal.reason === withdrawalReason),
        id: controller.signal.reason?.id,
      })
      finish()
    })
  }
}

function nativeScopeProof(events, documentId) {
  const own = events.filter(event => event.document === documentId && event.read === 1)
  const intents = own.filter(event => event.kind === 'departure-intent' && event.verified)
  const intent = intents[0]
  if (intents.length !== 1 || intent.type !== 'beforeunload' || !intent.trusted || !intent.nativeInterface
    || !intent.targetDocument || intent.targetWindow || !intent.currentWindow
    || !intent.documentCurrent || !intent.defaultViewMatches || !intent.topLevel)
    return false
  const starts = own.filter(event => event.kind === 'start')
  const attached = own.filter(event => event.kind === 'listener-attached' && event.active && event.listening)
  const removed = own.filter(event => event.kind === 'listener-removed' && event.listening === false)
  const withdrawals = own.filter(event => event.kind === 'owner-withdrawal' && event.id === intent.id && event.listening === false)
  const signals = own.filter(event => event.kind === 'signal-abort' && event.ownedReason && event.id === intent.id)
  const rejected = own.filter(event => event.kind === 'native-fetch-rejected')
  const settled = own.filter(event => event.kind === 'settled' && event.active === false && event.listening === false)
  return starts.length === 1 && attached.length === 1 && removed.length === 1
    && withdrawals.length === 1 && signals.length === 1 && rejected.length === 1 && settled.length === 1
    && rejected[0].signalAborted && rejected[0].sameAbortReason && rejected[0].ownedReason && rejected[0].id === intent.id
    && starts[0].sequence < attached[0].sequence && attached[0].sequence < intent.sequence
    && intent.sequence < removed[0].sequence && removed[0].sequence < withdrawals[0].sequence
    && withdrawals[0].sequence < signals[0].sequence && signals[0].sequence < rejected[0].sequence
    && rejected[0].sequence < settled[0].sequence
    && !own.some(event => ['native-headers', 'native-body', 'native-body-rejected'].includes(event.kind))
    && !own.some(event => event.kind === 'pagehide' && event.sequence < rejected[0].sequence)
}

function errorRecord(error) {
  if (error instanceof Error) {
    return {
      name: error.name,
      message: error.message,
      stack: error.stack,
      ...(error instanceof AggregateError ? { errors: error.errors.map(errorRecord) } : {}),
    }
  }
  if (error && typeof error === 'object' && 'error' in error && ('phase' in error || 'actor' in error))
    return error
  return { name: 'ThrownValue', message: String(error) }
}

async function writeEvidence(name, value, firstFailure, observationFailures = []) {
  try {
    await writeFile(output + '/' + name, JSON.stringify(value, null, 2))
  }
  catch (error) {
    throw new AggregateError([...(firstFailure ? [firstFailure] : []), ...observationFailures.filter(failure => failure !== firstFailure), ...cleanupFailures, error], 'Native reader evidence write failed')
  }
}

// Page.evaluate has no native timeout. A matcher timing out does not stop its
// pending protocol call; the controller must still reach independent closure.
async function observed(operation, stage) {
  let timer
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('Observation deadline: ' + stage)), 7000)
      }),
    ])
  }
  finally {
    clearTimeout(timer)
  }
}

async function observe(browser, engine) {
  const reader = 'beforeunload-scope'
  const scenario = 'committed'
  const events = []
  const received = []
  const receiptWrites = new Set()
  const downloads = []
  const pageErrors = []
  const observationFailures = []
  const ownedFailure = (actor, error) => cleanupFailures.push({ engine, reader, scenario, actor, error: errorRecord(error) })
  let firstFailure
  const rememberFailure = (phase, error) => {
    const failure = { phase, error: errorRecord(error) }
    firstFailure ??= failure
    observationFailures.push(failure)
  }
  let gate
  let context
  let page
  let held
  let destination
  let documentId
  let navigation
  let navigationKind = 'not-started'
  let navigationValidated = false
  let proofBeforeDestinationRelease = false
  let phase = 'backend setup'
  let httpClosedDuringObservation = false
  const cell = engine + '-beforeunload-scope'
  const stage = async (next) => {
    phase = next
    console.log(JSON.stringify({ control, engine, phase }))
    try {
      await writeFile(output + '/active.json', JSON.stringify({
        control, diagnosticOnly: true, productQualified: false, complete: false,
        completedCells: rows.length, expectedCells: expectedCells.length,
        engine, phase, received, firstFailure, observationFailures,
      }, null, 2))
    }
    catch (error) {
      ownedFailure('stage-write', error)
    }
  }
  const backend = createServer((request, response) => {
    if (request.url === '/api/installations') {
      response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }).end('[{"online":true}]')
      return
    }
    response.setHeader('content-type', 'text/html')
    if (request.url === '/live')
      response.end('<!doctype html><h1>Live</h1><script>(' + client.toString() + ')()</script>')
    else
      response.end('<!doctype html><h1>Destination</h1>')
  })
  const evaluate = (callback, argument) => observed(() => page.evaluate(callback, argument), 'document evaluation: ' + phase)
  const readJournal = async () => JSON.parse(await evaluate(() => sessionStorage.getItem('cairn-reader-mechanism') || '[]'))
  try {
    await stage('backend setup')
    backend.listen(0, '127.0.0.1')
    await once(backend, 'listening')
    const origin = 'http://127.0.0.1:' + backend.address().port
    gate = await nativeResponseGate(origin)
    context = await browser.newContext({
      ...(engine === 'webkit' ? { isMobile: true, hasTouch: true, viewport: { width: 390, height: 844 } } : {}),
      proxy: { server: gate.server },
    })
    context.setDefaultTimeout(7000)
    context.setDefaultNavigationTimeout(7000)
    await context.exposeBinding('readerReceipt', async (source, entry) => {
      let write
      try {
        assert.equal(source.page, page)
        assert.equal(source.frame, page.mainFrame())
        assert.equal(typeof entry.document, 'string')
        assert.equal(Number.isSafeInteger(entry.sequence), true)
        write = appendFile(output + '/' + cell + '.events.jsonl', JSON.stringify(entry) + '\n')
        receiptWrites.add(write)
        await write
        received.push(entry)
      }
      catch (error) {
        ownedFailure('receipt-write', error)
        throw error
      }
      finally {
        receiptWrites.delete(write)
      }
    })
    await context.tracing.start({ screenshots: false, snapshots: true, sources: true })
    page = await context.newPage()
    page.on('download', download => downloads.push(download))
    page.on('pageerror', error => pageErrors.push(errorRecord(error)))
    await stage('initial document')
    await page.goto(origin + '/live')
    documentId = await evaluate(() => window.readerDocument)
    held = gate.hold('availability')
    await evaluate(() => window.beginReader())
    await stage('held response ready')
    await expect.poll(() => {
      gate.check()
      return held.ready
    }, { timeout: 7000 }).toBe(true)
    assert.equal(held.status, 200)
    destination = gate.hold('agents')
    navigation = page.goto(origin + '/installations/fixture/agents')
    const arrival = navigation.then(() => 'committed', (error) => {
      rememberFailure('native Page.goto', error)
      return 'rejected'
    })
    await stage('destination response ready')
    await expect.poll(() => {
      gate.check()
      return destination.ready
    }, { timeout: 7000 }).toBe(true)
    assert.equal(destination.status, 200)
    await stage('trusted own-document beforeunload and actual native rejection')
    await expect.poll(() => nativeScopeProof(received, documentId), { timeout: 7000 }).toBe(true)
    proofBeforeDestinationRelease = true
    // The native terminal was observed while both responses were still held.
    // Release withdrawn bytes only after that proof, before committing HTML.
    held.release()
    destination.release()
    await stage('native navigation outcome')
    navigationKind = await observed(() => arrival, 'native navigation outcome')
    assert.equal(navigationKind, 'committed')
    await expect(page.getByRole('heading', { name: 'Destination', exact: true })).toBeVisible()
    assert.notEqual(await evaluate(() => window.readerDocument), documentId)
    navigationValidated = true
    await stage('positive HTTP close')
    await expect.poll(() => {
      gate.check()
      return held.closed && ['finished', 'cancelled'].includes(held.terminal)
    }, { timeout: 7000 }).toBe(true)
    httpClosedDuringObservation = true
  }
  catch (error) {
    rememberFailure(phase, error)
  }

  finally {
    held?.release()
    destination?.release()
    if (page && !page.isClosed()) {
      try {
        await stage('final journal acquisition')
        if (navigation && !navigationValidated)
          throw new Error('Final journal unavailable before unvalidated navigation closure')
        events.push(...await readJournal())
      }
      catch (error) {
        rememberFailure('journal read', error)
        ownedFailure('journal-read', error)
        events.push(...received)
      }
      try {
        await stage('page close')
        await observed(() => page.close(), 'page close')
      }
      catch (error) {
        ownedFailure('page', error)
      }
    }

    for (const download of downloads) {
      for (const operation of ['cancel', 'delete']) {
        try {
          await stage('download-' + operation)
          await observed(() => download[operation](), 'download-' + operation)
        }
        catch (error) {
          ownedFailure('download-' + operation, error)
        }
      }
    }

    if (context) {
      try {
        await stage('trace close')
        await observed(() => context.tracing.stop({ path: output + '/' + cell + '.zip' }), 'trace close')
      }
      catch (error) {
        ownedFailure('trace', error)
      }
      try {
        await stage('context close')
        await observed(() => context.close(), 'context close')
      }
      catch (error) {
        ownedFailure('context', error)
      }
    }

    try {
      await stage('receipt drain after context closure')
      await observed(() => Promise.all([...receiptWrites]), 'receipt drain')
    }
    catch (error) {
      ownedFailure('receipt-drain', error)
    }
    const captured = new Set(events.map(event => event.document + ':' + event.sequence))
    for (const entry of received) {
      const id = entry.document + ':' + entry.sequence
      if (!captured.has(id)) {
        events.push(entry)
        captured.add(id)
      }
    }
    events.sort((left, right) => left.sequence - right.sequence)

    if (gate) {
      try {
        await stage('gate close')
        await observed(() => gate.close(), 'gate close')
      }
      catch (error) {
        ownedFailure('gate', error)
      }
    }

    try {
      await stage('backend close')
      const closed = new Promise((resolve, reject) => backend.close(error => error ? reject(error) : resolve()))
      backend.closeAllConnections()
      await observed(() => closed, 'backend close')
    }
    catch (error) {
      ownedFailure('backend', error)
    }

    if (navigation) {
      try {
        await stage('navigation settlement after closure')
        await observed(() => navigation.catch(() => {}), 'navigation settlement after closure')
      }
      catch (error) {
        ownedFailure('navigation-close', error)
      }
    }
  }

  const ownFailures = cleanupFailures.filter(failure => failure.engine === engine)
  const row = {
    engine,
    reader,
    scenario,
    control,
    firstFailure,
    observationFailures,
    pageErrors,
    navigationKind,
    initialDocument: documentId,
    proofBeforeDestinationRelease,
    nativeScopeProven: nativeScopeProof(events, documentId),
    usable: proofBeforeDestinationRelease && nativeScopeProof(events, documentId)
      && navigationValidated && httpClosedDuringObservation && !firstFailure
      && !pageErrors.length && !ownFailures.length && !downloads.length,
    httpClosedDuringObservation,
    http: held && { ready: held.ready, status: held.status, closed: held.closed, terminal: held.terminal },
    ownedFailures: ownFailures,
    events,
  }
  rows.push(row)
  await writeEvidence(cell + '.json', { control, diagnosticOnly: true, productQualified: false, row, cleanupFailures }, firstFailure, observationFailures)
  await writeEvidence('observations.json', {
    control, diagnosticOnly: true, productQualified: false,
    gateSha256: createHash('sha256').update(gateBytes).digest('hex'), rows, cleanupFailures,
  }, firstFailure, observationFailures)
  console.log(JSON.stringify({ control, engine, usable: row.usable, failedPhase: firstFailure?.phase, nativeScopeProven: row.nativeScopeProven }))
}

engineLoop: for (const [engine, type] of [['chromium', chromium], ['webkit', webkit]]) {
  if (cleanupFailures.length)
    break
  let browser
  let enginePhase = 'browser launch'
  try {
    await writeEvidence('active.json', {
      control, diagnosticOnly: true, productQualified: false, complete: false,
      engine, phase: enginePhase, completedCells: rows.length, expectedCells: expectedCells.length,
    })
    browser = await type.launch()
    enginePhase = 'scope observation'
    await observe(browser, engine)
    // This first-stage experiment ends on a failed prerequisite; it does not
    // repeat the acquired baseline matrix or silently qualify an unrun engine.
    if (!rows.at(-1).usable || cleanupFailures.length)
      break engineLoop
  }
  catch (error) {
    engineFailures.push({ engine, phase: enginePhase, error: errorRecord(error) })
    break engineLoop
  }
  finally {
    if (browser) {
      try {
        await observed(() => browser.close(), 'browser close')
      }
      catch (error) {
        cleanupFailures.push({ engine, actor: 'browser', error: errorRecord(error) })
      }
    }
  }
}

const observedCells = rows.map(row => row.engine + ':beforeunload-scope').sort()
const allCellsObserved = JSON.stringify(observedCells) === JSON.stringify(expectedCells)
const allPageErrorsStrict = rows.every(row => row.pageErrors.length === 0)
const nativeScopeEvidenceComplete = allCellsObserved && rows.every(row => row.usable)
  && allPageErrorsStrict && cleanupFailures.length === 0 && engineFailures.length === 0
const result = {
  control,
  diagnosticOnly: true,
  productQualified: false,
  retainedPollingQualified: false,
  bfCacheQualified: false,
  genuineFailureHandlingQualified: false,
  nativeScopeEvidenceComplete,
  allCellsObserved,
  allPageErrorsStrict,
  expectedCells,
  observedCells,
  unobservedCells: expectedCells.filter(cell => !observedCells.includes(cell)),
  rows: rows.length,
  observations: rows,
  engineFailures,
  cleanupFailures,
}
await writeEvidence('result.json', result, engineFailures[0], engineFailures)
console.log(JSON.stringify({ ...result, observations: undefined, engineFailures: engineFailures.length, cleanupFailures: cleanupFailures.length }))
if (!nativeScopeEvidenceComplete)
  process.exitCode = 1
