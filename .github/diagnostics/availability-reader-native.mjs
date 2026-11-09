import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { stripTypeScriptTypes } from 'node:module'
import { chromium, expect, webkit } from '@playwright/test'

const output = 'test-results/reader-native'
await mkdir(output, { recursive: true })
const gateBytes = await readFile('tests/e2e/native-response-gate.ts')
const gateModule = Buffer.from(stripTypeScriptTypes(gateBytes.toString())).toString('base64')
const { nativeResponseGate } = await import('data:text/javascript;base64,' + gateModule)

// An isolated platform control, without Cairn accounts, services or credentials.
// A passing control does not qualify the application or a product repair.
function client(reader, scenario) {
  const key = 'cairn-reader-mechanism'
  const documentId = crypto.randomUUID()
  const pending = new Map()
  let hidden = false
  const record = (kind, read, detail = {}) => {
    const journal = JSON.parse(sessionStorage.getItem(key) || '[]')
    journal.push({
      document: documentId,
      read,
      kind,
      hidden,
      ...detail,
    })
    sessionStorage.setItem(key, JSON.stringify(journal))
  }

  sessionStorage.setItem(key, '[]')
  window.readerDocument = documentId
  window.addEventListener('pagehide', () => {
    hidden = true
    record('pagehide', 0)
    for (const controller of [...pending.values()])
      controller.abort()
  })
  window.abortReader = () => pending.get(1)?.abort()
  window.beginReader = (read = 1) => {
    const controller = new AbortController()
    pending.set(read, controller)
    record('start', read)
    controller.signal.addEventListener('abort', () => record('signal-abort', read, { signalAborted: true }), { once: true })
    let settled = false
    const finish = (kind, detail = {}) => {
      if (settled)
        return
      settled = true
      pending.delete(read)
      record(kind, read, { signalAborted: controller.signal.aborted, ...detail })
      const nativeCancellation = kind === 'native-abort' && detail.trusted && detail.exactTarget && !detail.explicitAbort && !controller.signal.aborted
      if (scenario === 'download' && read === 1 && nativeCancellation && !hidden) {
        setTimeout(() => {
          if (!hidden)
            window.beginReader(2)
        }, 3000)
      }
    }

    if (reader === 'fetch') {
      void fetch('/api/installations', { credentials: 'same-origin', keepalive: true, signal: controller.signal }).then(async (response) => {
        record('headers', read, { status: response.status })
        if (!response.ok) {
          finish('http-error', { status: response.status })
          return
        }

        try {
          const value = await response.json()
          finish('body', { status: response.status, online: value[0]?.online })
        }
        catch {
          finish('body-error')
        }
      }, () => finish('fetch-error'))

      return
    }

    const request = new XMLHttpRequest()
    let explicitAbort = false
    let headersRecorded = false
    const ownerAbort = () => {
      explicitAbort = true
      record('caller-abort', read, { signalAborted: controller.signal.aborted })
      request.abort()
      finish('owner-cancelled', { explicitAbort: true })
    }

    controller.signal.addEventListener('abort', ownerAbort, { once: true })
    request.onreadystatechange = () => {
      record('ready-state', read, {
        readyState: request.readyState,
        status: request.status,
        explicitAbort,
        signalAborted: controller.signal.aborted,
      })
      if (request.readyState >= 2 && request.status && !headersRecorded) {
        headersRecorded = true
        record('headers', read, { status: request.status })
      }
    }

    request.onload = () => {
      if (request.status !== 200) {
        finish('http-error', { status: request.status })
        return
      }

      try {
        const value = JSON.parse(request.responseText)
        finish('body', { status: request.status, online: value[0]?.online })
      }
      catch {
        finish('body-error')
      }
    }

    request.onerror = (event) => {
      record('platform-error', read, { trusted: event.isTrusted, explicitAbort, status: request.status })
      finish('error', { status: request.status })
    }

    request.ontimeout = (event) => {
      record('platform-timeout', read, { trusted: event.isTrusted, explicitAbort, status: request.status })
      finish('timeout', { status: request.status })
    }

    request.onabort = (event) => {
      record('platform-abort', read, {
        trusted: event.isTrusted,
        exactTarget: event.target === request,
        explicitAbort,
        signalAborted: controller.signal.aborted,
        status: request.status,
      })
      finish(explicitAbort ? 'owner-cancelled' : 'native-abort', {
        trusted: event.isTrusted,
        exactTarget: event.target === request,
        explicitAbort,
        status: request.status,
      })
    }

    try {
      request.open('GET', '/api/installations')
      request.send()
    }
    catch {
      finish('sync-error')
    }
  }
}

const terminalKinds = ['body', 'fetch-error', 'http-error', 'body-error', 'error', 'timeout', 'native-abort', 'owner-cancelled', 'sync-error']
const terminal = event => terminalKinds.includes(event.kind)
const scenarios = ['success', 'committed', 'download', 'owner-abort', 'network-error', 'truncated', 'http503', 'malformed', 'access-check']
const rows = []
const cleanupFailures = []

function errorRecord(error) {
  return error instanceof Error
    ? { name: error.name, message: error.message, stack: error.stack }
    : { name: 'ThrownValue', message: String(error) }
}

async function writeEvidence(name, value, firstFailure) {
  try {
    await writeFile(output + '/' + name, JSON.stringify(value, null, 2))
  }
  catch (error) {
    throw new AggregateError([...(firstFailure ? [firstFailure] : []), ...cleanupFailures, error], 'Native reader evidence write failed')
  }
}

async function observe(browser, engine, reader, scenario) {
  const events = []
  const downloads = []
  const pageErrors = []
  const ownedFailure = (actor, error) => cleanupFailures.push({
    engine,
    reader,
    scenario,
    actor,
    error: errorRecord(error),
  })
  let gate
  let context
  let page
  let held
  let destination
  let navigation
  let navigationKind = 'not-started'
  let identityRetained = false
  let firstFailure
  let phase = 'backend setup'
  let httpClosedDuringObservation = false
  const backend = createServer((request, response) => {
    if (request.url === '/api/installations') {
      if (scenario === 'network-error') {
        request.socket.destroy()
        return
      }

      if (scenario === 'truncated') {
        response.writeHead(200, {
          'content-type': 'application/json',
          'content-length': '200',
          'connection': 'close',
        })
        response.end('[{"online":')
        return
      }

      response.writeHead(scenario === 'http503' ? 503 : 200, { 'content-type': 'application/json' })
      response.end(scenario === 'malformed' ? '{invalid' : '[{"online":true}]')
      return
    }

    if (request.url === '/live') {
      if (scenario === 'access-check')
        response.setHeader('content-security-policy', "connect-src 'none'")
      const program = '(' + client.toString() + ')(' + JSON.stringify(reader) + ',' + JSON.stringify(scenario) + ')'
      response.setHeader('content-type', 'text/html')
      response.end('<!doctype html><h1>Live</h1><script>' + program + '</script>')
      return
    }

    response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Destination</h1>')
  })

  const readJournal = async () => JSON.parse(await page.evaluate(() => sessionStorage.getItem('cairn-reader-mechanism') || '[]'))

  try {
    backend.listen(0, '127.0.0.1')
    await once(backend, 'listening')
    const origin = 'http://127.0.0.1:' + backend.address().port
    const controlled = ['success', 'committed', 'download', 'owner-abort'].includes(scenario)
    if (controlled)
      gate = await nativeResponseGate(origin)
    context = await browser.newContext({
      ...(engine === 'webkit' ? { isMobile: true, hasTouch: true, viewport: { width: 390, height: 844 } } : {}),
      ...(gate ? { proxy: { server: gate.server } } : {}),
    })
    await context.tracing.start({ screenshots: false, snapshots: true, sources: true })
    page = await context.newPage()
    page.on('download', download => downloads.push(download))
    page.on('pageerror', error => pageErrors.push(errorRecord(error)))
    phase = 'initial document'
    await page.goto(origin + '/live')
    const documentId = await page.evaluate(() => window.readerDocument)
    if (controlled)
      held = gate.hold('availability')
    await page.evaluate(() => window.beginReader())
    if (controlled) {
      phase = 'held response ready'
      await expect.poll(() => {
        gate.check()
        return held.ready
      }, { timeout: 7000 }).toBe(true)
      assert.equal(held.status, 200)
      if (scenario === 'owner-abort') {
        await page.evaluate(() => window.abortReader())
      }
      else if (scenario !== 'success') {
        destination = gate.hold('agents')
        navigation = page.goto(origin + '/installations/fixture/agents')
        const arrival = navigation.then(() => 'committed', () => 'rejected')
        phase = 'navigation response ready'
        await expect.poll(() => {
          gate.check()
          return destination.ready
        }, { timeout: 7000 }).toBe(true)
        assert.equal(destination.status, 200)
        if (scenario === 'download')
          destination.downloadNavigation()
        else
          destination.release()
        navigationKind = await arrival
        phase = 'native navigation outcome'
        if (scenario === 'download') {
          await expect.poll(() => downloads.length, { timeout: 7000 }).toBe(1)
          assert.equal(downloads[0].suggestedFilename(), 'availability-navigation.html')
          identityRetained = await page.evaluate(() => window.readerDocument) === documentId
          assert.equal(identityRetained, true)
          await expect(page.getByRole('heading', { name: 'Live', exact: true })).toBeVisible()
        }
        else {
          assert.equal(navigationKind, 'committed')
          await expect(page.getByRole('heading', { name: 'Destination', exact: true })).toBeVisible()
        }
      }
      held.release()
    }

    phase = 'exact read terminal and replacement'
    await expect.poll(async () => {
      const journal = await readJournal()
      const first = journal.filter(event => event.read === 1 && terminal(event))
      if (scenario === 'committed' && journal.some(event => event.read === 1 && event.kind === 'signal-abort' && event.hidden))
        return true
      if (scenario === 'download' && first.some(event => event.kind === 'native-abort'))
        return journal.some(event => event.read === 2 && terminal(event))
      return first.length > 0
    }, { timeout: 7000 }).toBe(true)
    if (held) {
      phase = 'positive HTTP close'
      await expect.poll(() => {
        gate.check()
        return held.closed && ['finished', 'cancelled'].includes(held.terminal)
      }, { timeout: 7000 }).toBe(true)
      httpClosedDuringObservation = true
    }
  }
  catch (error) {
    firstFailure = { phase, error: errorRecord(error) }
  }
  finally {
    held?.release()
    destination?.release()
    if (page && !page.isClosed()) {
      try {
        events.push(...await readJournal())
      }
      catch (error) {
        firstFailure ??= { phase: 'journal read', error: errorRecord(error) }
      }
      try {
        await page.close()
      }
      catch (error) {
        ownedFailure('page', error)
      }
    }

    for (const download of downloads) {
      for (const operation of ['cancel', 'delete']) {
        try {
          await download[operation]()
        }
        catch (error) {
          ownedFailure('download-' + operation, error)
        }
      }
    }

    if (context) {
      try {
        await context.tracing.stop({ path: output + '/' + engine + '-' + reader + '-' + scenario + '.zip' })
      }
      catch (error) {
        ownedFailure('trace', error)
      }
      try {
        await context.close()
      }
      catch (error) {
        ownedFailure('context', error)
      }
    }

    if (gate) {
      try {
        await gate.close()
      }
      catch (error) {
        ownedFailure('gate', error)
      }
    }

    try {
      const closed = new Promise((resolve, reject) => backend.close(error => error ? reject(error) : resolve()))
      backend.closeAllConnections()
      await closed
    }
    catch (error) {
      ownedFailure('backend', error)
    }

    await navigation?.catch(() => {})
  }

  const first = events.filter(event => event.read === 1 && terminal(event))
  const trustedNativeCancel = first.some(event => event.kind === 'native-abort' && event.trusted && event.exactTarget && !event.explicitAbort && !event.signalAborted)
  const freshBody = events.filter(event => event.read === 2 && event.kind === 'body' && event.status === 200 && event.online === true).length === 1
  const originalBody = first.filter(event => event.kind === 'body' && event.status === 200 && event.online === true).length === 1
  const ownedWithdrawal = events.some(event => event.read === 1 && event.kind === 'signal-abort' && event.hidden && event.signalAborted)
  const unexpectedNativeError = ['success', 'committed', 'download', 'owner-abort'].includes(scenario)
    && events.some(event => ['platform-error', 'platform-timeout'].includes(event.kind))
  let usable = false
  if (scenario === 'success')
    usable = originalBody && first.length === 1 && !trustedNativeCancel
  else if (scenario === 'committed')
    usable = (ownedWithdrawal || trustedNativeCancel) && !first.some(event => ['error', 'fetch-error', 'body-error', 'timeout', 'sync-error'].includes(event.kind) && !event.signalAborted)
  else if (scenario === 'download')
    usable = identityRetained && (trustedNativeCancel ? freshBody && !originalBody : originalBody)
  else if (scenario === 'owner-abort')
    usable = first.some(event => event.signalAborted && ['owner-cancelled', 'fetch-error'].includes(event.kind)) && !trustedNativeCancel
  else if (scenario === 'http503')
    usable = first.some(event => event.kind === 'http-error' && event.status === 503)
  else if (scenario === 'malformed')
    usable = first.some(event => event.kind === 'body-error')
  else
    usable = first.some(event => ['error', 'fetch-error', 'body-error', 'sync-error'].includes(event.kind)) && !trustedNativeCancel && !originalBody
  const row = {
    engine,
    reader,
    scenario,
    firstFailure,
    pageErrors,
    navigationKind,
    identityRetained,
    usable: usable && !firstFailure && !pageErrors.length && !unexpectedNativeError && (!held || httpClosedDuringObservation),
    httpClosedDuringObservation,
    http: held && { closed: held.closed, terminal: held.terminal },
    events,
  }
  rows.push(row)
  await writeEvidence(engine + '-' + reader + '-' + scenario + '.json', {
    diagnosticOnly: true,
    productQualified: false,
    row,
    cleanupFailures,
  }, firstFailure)
  await writeEvidence('observations.json', {
    diagnosticOnly: true,
    productQualified: false,
    gateSha256: createHash('sha256').update(gateBytes).digest('hex'),
    rows,
    cleanupFailures,
  }, firstFailure)
  console.log(JSON.stringify({
    engine,
    reader,
    scenario,
    usable: row.usable,
    failedPhase: firstFailure?.phase,
    terminal: first.map(event => event.kind),
  }))
}

for (const [engine, type] of [['chromium', chromium], ['webkit', webkit]]) {
  let browser
  try {
    browser = await type.launch()
    for (const reader of ['fetch', 'xhr']) {
      for (const scenario of scenarios)
        await observe(browser, engine, reader, scenario)
    }
  }
  finally {
    if (browser) {
      try {
        await browser.close()
      }
      catch (error) {
        cleanupFailures.push({ engine, actor: 'browser', error: errorRecord(error) })
      }
    }
  }
}

const xhrControlsPassed = rows.length === 36 && rows.filter(row => row.reader === 'xhr').every(row => row.usable) && cleanupFailures.length === 0
await writeEvidence('result.json', {
  diagnosticOnly: true,
  productQualified: false,
  xhrControlsPassed,
  rows: rows.length,
  cleanupFailures,
})
console.log(JSON.stringify({
  diagnosticOnly: true,
  productQualified: false,
  xhrControlsPassed,
  rows: rows.length,
  cleanupFailures: cleanupFailures.length,
}))
if (!xhrControlsPassed)
  process.exitCode = 1
