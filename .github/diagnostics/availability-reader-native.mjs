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

// This profile tests only native XHR ownership and its attribution negatives.
// It does not qualify retained polling, BFCache, the application or a repair.
const control = 'beforeunload-xhr-owned-terminal-only'
const plannedCases = [
  { engine: 'chromium', scenario: 'owned-abort' },
  { engine: 'webkit', scenario: 'owned-abort' },
  { engine: 'webkit', scenario: 'failure-first' },
  { engine: 'webkit', scenario: 'armed-outside-call' },
]
const expectedCells = plannedCases.map(cell => cell.engine + ':' + cell.scenario).sort()
const rows = []
const cleanupFailures = []
const engineFailures = []

function client(scenario) {
  const key = 'cairn-reader-mechanism'
  const capturedWindow = window
  const capturedDocument = document
  const documentId = crypto.randomUUID()
  const request = new XMLHttpRequest()
  const nativeAbort = capturedWindow.XMLHttpRequest.prototype.abort
  const controller = new AbortController()
  let started = false
  let settled = false
  let terminalKind
  let listening = false
  let insideOwnedAbortCall = false
  let ownedReason
  let eventId = 0
  let resolveOutcome
  const result = new Promise(resolve => resolveOutcome = resolve)
  const record = (kind, detail = {}) => {
    const journal = JSON.parse(sessionStorage.getItem(key) || '[]')
    const entry = { document: documentId, read: 1, kind, sequence: journal.length + 1, time: performance.now(), ...detail }
    journal.push(entry)
    sessionStorage.setItem(key, JSON.stringify(journal))
    void capturedWindow.readerReceipt?.(entry).catch(() => {})
  }
  const ownerArmed = () => Boolean(ownedReason && controller.signal.reason === ownedReason)
  const removeListener = () => {
    if (!listening)
      return
    capturedWindow.removeEventListener('beforeunload', beforeUnload)
    listening = false
    record('listener-removed', { listening })
  }
  const settle = (kind, source) => {
    if (settled) {
      record('later-terminal-refused', { prior: terminalKind, attempted: kind })
      return
    }
    settled = true
    terminalKind = kind
    removeListener()
    // This diagnostic caller promise has no built-in XHR equivalent. It only
    // settles in the actual native terminal handler, never before abort().
    resolveOutcome({ kind, source })
    record('typed-read-settled', { terminalKind: kind, source, insideOwnedAbortCall, listening })
  }
  void result.then(outcome => record('caller-outcome', { terminalKind: outcome.kind, source: outcome.source }))
  function beforeUnload(event) {
    const id = ++eventId
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
    if (!verified || settled || !started)
      return
    // Validation precedes removal, so synthetic dispatch cannot disarm it.
    removeListener()
    ownedReason = { kind: 'beforeunload', id, document: documentId, read: 1 }
    record('owner-armed', { id, listening })
    controller.abort(ownedReason)
  }
  controller.signal.addEventListener('abort', () => {
    record('signal-abort', { ownerArmed: ownerArmed(), id: controller.signal.reason?.id })
    if (settled) {
      record('abort-after-terminal-refused', { prior: terminalKind })
      return
    }
    if (scenario === 'armed-outside-call' || !ownerArmed()) {
      // Real native attribution negative: armed state, but no owned bracket.
      record('outside-abort-call-enter', { ownerArmed: ownerArmed(), insideOwnedAbortCall })
      try {
        nativeAbort.call(request)
      }
      finally {
        record('outside-abort-call-return', { insideOwnedAbortCall })
      }
      return
    }
    insideOwnedAbortCall = true
    record('owned-abort-call-enter', { id: ownedReason.id, insideOwnedAbortCall })
    try {
      nativeAbort.call(request)
    }
    finally {
      insideOwnedAbortCall = false
      record('owned-abort-call-return', { id: ownedReason.id, insideOwnedAbortCall })
    }
  }, { once: true })
  capturedWindow.addEventListener('pagehide', (event) => {
    record('pagehide', { persisted: event.persisted })
    removeListener()
    if (started && !settled)
      controller.abort()
  })
  const nativeEvent = event => ({
    type: event.type,
    trusted: event.isTrusted,
    nativeInterface: typeof capturedWindow.ProgressEvent === 'function' && event instanceof capturedWindow.ProgressEvent,
    exactTarget: event.target === request,
    currentRequest: event.currentTarget === request,
    insideOwnedAbortCall,
    ownerArmed: ownerArmed(),
    id: controller.signal.reason?.id,
    settledBefore: settled,
    readyState: request.readyState,
    status: request.status,
  })
  const validNative = (meta, type) => meta.type === type && meta.trusted && meta.nativeInterface && meta.exactTarget && meta.currentRequest
  request.onabort = (event) => {
    const meta = nativeEvent(event)
    record('native-abort', meta)
    if (!validNative(meta, 'abort'))
      return
    const owned = meta.insideOwnedAbortCall && meta.ownerArmed && !meta.settledBefore
    settle(owned ? 'owned-abort' : 'unattributed-abort', 'native-onabort')
  }
  request.onerror = (event) => {
    const meta = nativeEvent(event)
    record('native-error', meta)
    if (validNative(meta, 'error'))
      settle('error', 'native-onerror')
  }
  request.ontimeout = (event) => {
    const meta = nativeEvent(event)
    record('native-timeout', meta)
    if (validNative(meta, 'timeout'))
      settle('timeout', 'native-ontimeout')
  }
  request.onload = (event) => {
    const meta = nativeEvent(event)
    record('native-load', meta)
    if (!validNative(meta, 'load'))
      return
    if (request.status !== 200) {
      settle('http-error', 'native-onload')
      return
    }
    try {
      const value = JSON.parse(request.responseText)
      record('native-body', { exactBody: JSON.stringify(value) === '[{"online":true}]' })
      settle('body', 'native-onload')
    }
    catch {
      settle('body-error', 'native-onload')
    }
  }
  request.onreadystatechange = () => record('ready-state', {
    readyState: request.readyState, status: request.status, insideOwnedAbortCall, ownerArmed: ownerArmed(),
  })
  capturedWindow.readerDocument = documentId
  capturedWindow.beginReader = () => {
    if (started)
      throw new Error('The scope control owns exactly one read')
    started = true
    record('start')
    try {
      request.open('GET', '/api/installations')
      capturedWindow.addEventListener('beforeunload', beforeUnload)
      listening = true
      record('listener-attached', { listening, withCredentials: request.withCredentials })
      request.send()
    }
    catch {
      settle('sync-error', 'native-api-throw')
    }
  }
  capturedWindow.abortCallerAfterFailure = () => {
    if (!settled || terminalKind !== 'error')
      throw new Error('The earlier-failure negative needs its native error first')
    record('late-caller-abort', { prior: terminalKind })
    controller.abort()
  }
  capturedWindow.readerState = () => ({ settled, terminalKind, signalAborted: controller.signal.aborted, listening, insideOwnedAbortCall })
}

function nativeTerminalFacts(events, documentId) {
  const own = events.filter(event => event.document === documentId && event.read === 1)
  const intents = own.filter(event => event.kind === 'departure-intent' && event.verified)
  const intent = intents[0]
  const windowScope = intents.length === 1 && intent.type === 'beforeunload' && intent.trusted && intent.nativeInterface
    && intent.targetDocument && !intent.targetWindow && intent.currentWindow
    && intent.documentCurrent && intent.defaultViewMatches && intent.topLevel
  const one = kind => own.filter(event => event.kind === kind)
  const removed = one('listener-removed')
  const armed = one('owner-armed')
  const signals = one('signal-abort')
  const validEvent = (event, type) => event.type === type && event.trusted && event.nativeInterface && event.exactTarget && event.currentRequest
  const native = one('native-abort').filter(event => validEvent(event, 'abort'))
  const starts = one('start')
  const attached = one('listener-attached')
  const terminals = one('typed-read-settled')
  const callers = one('caller-outcome')
  const enter = one('owned-abort-call-enter')
  const returned = one('owned-abort-call-return')
  const outside = one('outside-abort-call-enter')
  const outsideReturn = one('outside-abort-call-return')
  const nativeAbort = native.length === 1 && native[0].type === 'abort' && native[0].trusted && native[0].nativeInterface
    && native[0].exactTarget && native[0].currentRequest && !native[0].settledBefore
  const terminal = terminals[0]
  const common = Boolean(windowScope && starts.length === 1 && attached.length === 1
    && starts[0].sequence < attached[0].sequence && attached[0].sequence < intent.sequence
    && removed.length === 1 && removed[0].listening === false && armed.length === 1 && armed[0].id === intent.id && armed[0].listening === false
    && signals.length === 1 && signals[0].ownerArmed && signals[0].id === intent.id
    && intent.sequence < removed[0].sequence && removed[0].sequence < armed[0].sequence
    && armed[0].sequence < signals[0].sequence && nativeAbort && native[0].ownerArmed && native[0].id === intent.id
    && terminals.length === 1 && callers.length === 1 && terminal.source === 'native-onabort' && terminal.listening === false
    && native[0].sequence < terminal.sequence && terminal.sequence < callers[0].sequence
    && !own.some(event => event.kind === 'native-body'
      || (event.kind === 'native-error' && validEvent(event, 'error'))
      || (event.kind === 'native-timeout' && validEvent(event, 'timeout'))
      || (event.kind === 'native-load' && validEvent(event, 'load'))))
  const ownedAbortProven = Boolean(common && enter.length === 1 && returned.length === 1 && !outside.length
    && native[0].insideOwnedAbortCall && terminal.insideOwnedAbortCall && terminal.terminalKind === 'owned-abort'
    && callers[0].terminalKind === 'owned-abort' && enter[0].insideOwnedAbortCall && enter[0].id === intent.id && returned[0].id === intent.id
    && signals[0].sequence < enter[0].sequence && enter[0].sequence < native[0].sequence
    && terminal.sequence < returned[0].sequence && returned[0].insideOwnedAbortCall === false)
  const attributionRefused = Boolean(common && outside.length === 1 && outsideReturn.length === 1 && !enter.length && !returned.length
    && !native[0].insideOwnedAbortCall && !terminal.insideOwnedAbortCall && terminal.terminalKind === 'unattributed-abort'
    && callers[0].terminalKind === 'unattributed-abort' && outside[0].ownerArmed && !outside[0].insideOwnedAbortCall
    && signals[0].sequence < outside[0].sequence && outside[0].sequence < native[0].sequence
    && terminal.sequence < outsideReturn[0].sequence && !outsideReturn[0].insideOwnedAbortCall)
  const errors = one('native-error').filter(event => validEvent(event, 'error'))
  const late = one('late-caller-abort')
  const ignored = one('abort-after-terminal-refused')
  const failureFirstPreserved = Boolean(starts.length === 1 && attached.length === 1 && removed.length === 1 && !removed[0].listening
    && errors.length === 1 && starts[0].sequence < attached[0].sequence && attached[0].sequence < errors[0].sequence
    && errors[0].type === 'error' && errors[0].trusted
    && errors[0].nativeInterface && errors[0].exactTarget && errors[0].currentRequest && !errors[0].settledBefore
    && terminals.length === 1 && terminal.terminalKind === 'error' && terminal.source === 'native-onerror'
    && callers.length === 1 && callers[0].terminalKind === 'error' && !native.length && !enter.length && !armed.length
    && late.length === 1 && late[0].prior === 'error' && ignored.length === 1 && ignored[0].prior === 'error'
    && signals.length === 1 && !signals[0].ownerArmed && errors[0].sequence < removed[0].sequence && removed[0].sequence < terminal.sequence
    && terminal.sequence < callers[0].sequence && callers[0].sequence < late[0].sequence
    && late[0].sequence < signals[0].sequence && signals[0].sequence < ignored[0].sequence)
  return { ownedAbortProven, attributionRefused, failureFirstPreserved }
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

async function observe(browser, engine, scenario) {
  const reader = 'scoped-xhr'
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
  let activeDocumentPreserved = false
  let negativeValidated = false
  const failedSockets = new Set()
  let failedBackendRequests = 0
  let phase = 'backend setup'
  let httpClosedDuringObservation = false
  const cell = engine + '-' + scenario
  const stage = async (next) => {
    phase = next
    console.log(JSON.stringify({ control, engine, phase }))
    try {
      await writeFile(output + '/active.json', JSON.stringify({
        control, diagnosticOnly: true, productQualified: false, complete: false,
        completedCells: rows.length, expectedCells: expectedCells.length,
        engine, scenario, phase, received, firstFailure, observationFailures,
      }, null, 2))
    }
    catch (error) {
      ownedFailure('stage-write', error)
    }
  }
  const backend = createServer((request, response) => {
    if (request.url === '/api/installations') {
      if (scenario === 'failure-first') {
        failedBackendRequests++
        failedSockets.add(request.socket)
        request.socket.once('close', () => failedSockets.delete(request.socket))
        request.socket.destroy()
        return
      }
      response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }).end('[{"online":true}]')
      return
    }
    response.setHeader('content-type', 'text/html')
    if (request.url === '/live')
      response.end('<!doctype html><h1>Live</h1><script>(' + client.toString() + ')(' + JSON.stringify(scenario) + ')</script>')
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
    // Expected native network loss uses the direct backend: the response gate
    // correctly treats upstream failure as an infrastructure error.
    if (scenario !== 'failure-first')
      gate = await nativeResponseGate(origin)
    context = await browser.newContext({
      ...(engine === 'webkit' ? { isMobile: true, hasTouch: true, viewport: { width: 390, height: 844 } } : {}),
      ...(gate ? { proxy: { server: gate.server } } : {}),
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
    if (gate)
      held = gate.hold('availability')
    await evaluate(() => window.beginReader())
    if (scenario === 'failure-first') {
      await stage('genuine native XHR error before caller abort')
      await expect.poll(() => received.some(event => event.document === documentId && event.kind === 'caller-outcome' && event.terminalKind === 'error'), { timeout: 7000 }).toBe(true)
      await evaluate(() => window.abortCallerAfterFailure())
      await stage('earlier native failure remains the only caller outcome')
      await expect.poll(() => nativeTerminalFacts(received, documentId).failureFirstPreserved, { timeout: 7000 }).toBe(true)
      const state = await evaluate(() => window.readerState())
      assert.deepEqual(state, { settled: true, terminalKind: 'error', signalAborted: true, listening: false, insideOwnedAbortCall: false })
      assert.equal(await evaluate(() => window.readerDocument), documentId)
      await expect(page.getByRole('heading', { name: 'Live', exact: true })).toBeVisible()
      activeDocumentPreserved = true
      await expect.poll(() => failedBackendRequests > 0 && failedSockets.size === 0, { timeout: 7000 }).toBe(true)
      negativeValidated = true
    }
    else {
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
      await stage('trusted native XHR terminal in its required call bracket')
      await expect.poll(() => {
        const facts = nativeTerminalFacts(received, documentId)
        return scenario === 'owned-abort' ? facts.ownedAbortProven : facts.attributionRefused
      }, { timeout: 7000 }).toBe(true)
      proofBeforeDestinationRelease = true
      // The actual native XHR event was observed while both responses were held.
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

  const ownFailures = cleanupFailures.filter(failure => failure.engine === engine && failure.scenario === scenario)
  const facts = nativeTerminalFacts(events, documentId)
  const expectationMatched = scenario === 'owned-abort'
    ? facts.ownedAbortProven && proofBeforeDestinationRelease && navigationValidated && httpClosedDuringObservation
    : scenario === 'armed-outside-call'
      ? facts.attributionRefused && proofBeforeDestinationRelease && navigationValidated && httpClosedDuringObservation
      : facts.failureFirstPreserved && activeDocumentPreserved && negativeValidated
  const row = {
    engine, reader, scenario, control,
    firstFailure, observationFailures, pageErrors, navigationKind, initialDocument: documentId,
    facts, proofBeforeDestinationRelease, activeDocumentPreserved,
    expectedControlMatched: expectationMatched,
    usable: expectationMatched && !firstFailure && !pageErrors.length && !ownFailures.length && !downloads.length,
    httpClosedDuringObservation,
    http: held && { ready: held.ready, status: held.status, closed: held.closed, terminal: held.terminal },
    failedBackendRequests,
    ownedFailures: ownFailures,
    events,
  }
  rows.push(row)
  await writeEvidence(cell + '.json', { control, diagnosticOnly: true, productQualified: false, row, cleanupFailures }, firstFailure, observationFailures)
  await writeEvidence('observations.json', {
    control, diagnosticOnly: true, productQualified: false,
    gateSha256: createHash('sha256').update(gateBytes).digest('hex'), rows, cleanupFailures,
  }, firstFailure, observationFailures)
  console.log(JSON.stringify({ control, engine, scenario, usable: row.usable, failedPhase: firstFailure?.phase, facts }))
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
    for (const cell of plannedCases.filter(cell => cell.engine === engine)) {
      enginePhase = 'scope observation: ' + cell.scenario
      await observe(browser, engine, cell.scenario)
      if (!rows.at(-1).usable || cleanupFailures.length)
        break engineLoop
    }
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

const observedCells = rows.map(row => row.engine + ':' + row.scenario).sort()
const allCellsObserved = JSON.stringify(observedCells) === JSON.stringify(expectedCells)
const allPageErrorsStrict = rows.every(row => row.pageErrors.length === 0)
const nativeOwnershipEvidenceComplete = allCellsObserved && rows.every(row => row.usable)
  && allPageErrorsStrict && cleanupFailures.length === 0 && engineFailures.length === 0
const result = {
  control,
  diagnosticOnly: true,
  productQualified: false,
  retainedPollingQualified: false,
  bfCacheQualified: false,
  genuineFailureHandlingQualified: false,
  nativeOwnershipEvidenceComplete,
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
if (!nativeOwnershipEvidenceComplete)
  process.exitCode = 1
