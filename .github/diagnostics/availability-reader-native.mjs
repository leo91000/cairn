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
  let resumeTimer
  let intentSequence = 0
  let failureCount = 0
  const record = (kind, read, detail = {}) => {
    const journal = JSON.parse(sessionStorage.getItem(key) || '[]')
    journal.push({
      document: documentId,
      read,
      kind,
      hidden,
      sequence: journal.length + 1,
      time: performance.now(),
      ...detail,
    })
    sessionStorage.setItem(key, JSON.stringify(journal))
  }

  if (!sessionStorage.getItem(key))
    sessionStorage.setItem(key, '[]')
  window.readerDocument = documentId
  window.addEventListener('pagehide', () => {
    hidden = true
    record('pagehide', 0)
    clearTimeout(resumeTimer)
    for (const controller of [...pending.values()])
      controller.abort()
  })
  window.abortReader = () => pending.get(1)?.abort()
  if (reader === 'intent-fetch') {
    const navigation = window.navigation
    record('navigation-api', 0, { available: Boolean(navigation?.addEventListener) })
    navigation?.addEventListener('navigate', (event) => {
      const id = ++intentSequence
      const recognized = typeof event.destination?.sameDocument === 'boolean'
      const verified = event.isTrusted && event.target === navigation && recognized && !event.destination.sameDocument
      record('navigation-intent', 0, {
        id,
        trusted: event.isTrusted,
        exactTarget: event.target === navigation,
        recognized,
        sameDocument: event.destination?.sameDocument,
        navigationType: event.navigationType,
        verified,
      })
      if (!verified)
        return
      for (const [read, controller] of pending) {
        record('owner-withdrawal', read, { id })
        controller.abort({ kind: 'navigation-intent', id })
      }
    })
  }
  window.beginReader = (read = 1) => {
    const controller = new AbortController()
    pending.set(read, controller)
    record('start', read)
    controller.signal.addEventListener('abort', () => record('signal-abort', read, {
      signalAborted: true,
      reason: controller.signal.reason?.kind,
      intentId: controller.signal.reason?.id,
    }), { once: true })
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

    if (reader === 'intent-fetch') {
      let abortListener
      const withdrawal = new Promise((_, reject) => {
        abortListener = () => reject(controller.signal.reason)
        controller.signal.addEventListener('abort', abortListener, { once: true })
      })
      const raw = fetch('/api/installations', {
        credentials: 'same-origin',
        keepalive: true,
        signal: controller.signal,
        headers: { 'x-cairn-diagnostic-read': String(read) },
      })
      // Observe the real native promise independently of the ownership race.
      // Register before race so a rejection already queued before intent stays
      // a genuine failure, even if its observer initiates a later navigation.
      void raw.then(() => record('native-fetch-fulfilled', read), (error) => {
        record('native-fetch-rejected', read, { sameAbortReason: controller.signal.aborted && error === controller.signal.reason })
        if (scenario === 'failure-before-intent' && read === 1 && window.navigation) {
          const result = window.navigation.navigate('/installations/fixture/agents')
          void result.finished.catch(() => {})
        }
      })
      void (async () => {
        try {
          const response = await Promise.race([raw, withdrawal])
          record('headers', read, { status: response.status })
          if (!response.ok) {
            failureCount++
            finish('http-error', { status: response.status })
            return
          }
          const body = response.json()
          void body.then(() => record('native-body-fulfilled', read), (error) => record('native-body-rejected', read, {
            sameAbortReason: controller.signal.aborted && error === controller.signal.reason,
          }))
          try {
            const value = await Promise.race([body, withdrawal])
            failureCount = 0
            finish('body', {
              status: response.status,
              online: value[0]?.online,
              exactBody: Array.isArray(value) && value.length === 1 && Object.keys(value[0] || {}).length === 1 && value[0]?.online === true,
            })
          }
          catch (error) {
            if (controller.signal.aborted && error === controller.signal.reason)
              finish('owner-cancelled', { reason: error?.kind, intentId: error?.id })
            else {
              failureCount++
              finish('body-error')
            }
          }
        }
        catch (error) {
          if (controller.signal.aborted && error === controller.signal.reason)
            finish('owner-cancelled', { reason: error?.kind, intentId: error?.id })
          else {
            failureCount++
            finish('fetch-error')
          }
        }
        finally {
          controller.signal.removeEventListener('abort', abortListener)
          // Same existing clean-poll cadence and failure backoff; no rearm from
          // the intent handler, and never resume while pagehide owns suspension.
          if (read === 1 && !hidden && ['committed', 'download', 'reload', 'delayed-commit', 'failure-before-intent'].includes(scenario)) {
            const delay = Math.min(15000, 3000 * 2 ** Math.min(failureCount, 3))
            record('resume-scheduled', read, { delay, failureCount })
            resumeTimer = setTimeout(() => {
              if (!hidden)
                window.beginReader(2)
            }, delay)
          }
        }
      })()
      return
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
      finish('owner-cancelled', { explicitAbort: true })
      request.abort()
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
      record('platform-error', read, {
        trusted: event.isTrusted,
        explicitAbort,
        signalAborted: controller.signal.aborted,
        status: request.status,
      })
      finish('error', { status: request.status })
    }

    request.ontimeout = (event) => {
      record('platform-timeout', read, {
        trusted: event.isTrusted,
        explicitAbort,
        signalAborted: controller.signal.aborted,
        status: request.status,
      })
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
const intentScenarios = [...scenarios, 'same-document', 'synthetic-intent', 'reload', 'delayed-commit', 'failure-before-intent', 'http401']
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
  let secondHeld
  let documentId
  let apiRequests = 0
  let navigation
  let navigationKind = 'not-started'
  let identityRetained = false
  let firstFailure
  let phase = 'backend setup'
  let httpClosedDuringObservation = false
  const backend = createServer((request, response) => {
    if (request.url === '/api/installations') {
      apiRequests++
      if (scenario === 'network-error' || (scenario === 'failure-before-intent' && request.headers['x-cairn-diagnostic-read'] === '1')) {
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

      const status = scenario === 'http503' ? 503 : scenario === 'http401' ? 401 : 200
      response.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store' })
      response.end(scenario === 'malformed' ? '{invalid' : '[{"online":true}]')
      return
    }

    if (request.url === '/live' || (scenario === 'reload' && request.url === '/installations/fixture/agents')) {
      if (scenario === 'access-check')
        response.setHeader('content-security-policy', "connect-src 'none'")
      const program = '(' + client.toString() + ')(' + JSON.stringify(reader) + ',' + JSON.stringify(scenario) + ')'
      response.setHeader('content-type', 'text/html')
      response.end('<!doctype html><h1>Live</h1><script>' + program + '</script>')
      return
    }

    if (scenario === 'failure-before-intent')
      response.setHeader('content-disposition', 'attachment; filename=availability-navigation.html')
    response.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><h1>Destination</h1>')
  })

  const readJournal = async () => JSON.parse(await page.evaluate(() => sessionStorage.getItem('cairn-reader-mechanism') || '[]'))

  try {
    backend.listen(0, '127.0.0.1')
    await once(backend, 'listening')
    const origin = 'http://127.0.0.1:' + backend.address().port
    const controlled = ['success', 'committed', 'download', 'owner-abort', 'same-document', 'synthetic-intent', 'reload', 'delayed-commit'].includes(scenario)
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
    await page.goto(origin + (scenario === 'reload' ? '/installations/fixture/agents' : '/live'))
    documentId = await page.evaluate(() => window.readerDocument)
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
      else if (scenario === 'same-document' || scenario === 'synthetic-intent') {
        await page.evaluate((kind) => {
          if (kind === 'same-document')
            history.pushState({ probe: true }, '', '#same-document')
          else {
            const event = new Event('navigate')
            Object.defineProperty(event, 'destination', { value: { sameDocument: false } })
            window.navigation?.dispatchEvent(event)
          }
        }, scenario)
        identityRetained = await page.evaluate(() => window.readerDocument) === documentId
        assert.equal(identityRetained, true)
        if (scenario === 'same-document')
          assert.equal(await page.evaluate(() => history.state?.probe === true && location.hash === '#same-document'), true)
      }
      else if (scenario !== 'success') {
        destination = gate.hold('agents')
        navigation = scenario === 'reload' ? page.reload() : page.goto(origin + '/installations/fixture/agents')
        const arrival = navigation.then(() => 'committed', () => 'rejected')
        phase = 'navigation response ready'
        await expect.poll(() => {
          gate.check()
          return destination.ready
        }, { timeout: 7000 }).toBe(true)
        assert.equal(destination.status, 200)
        if (reader === 'intent-fetch') {
          phase = 'owned intent and actual native rejection before destination release'
          await expect.poll(async () => {
            const journal = await readJournal()
            return journal.some(event => event.document === documentId && event.read === 1 && event.kind === 'native-fetch-rejected' && event.sameAbortReason)
          }, { timeout: 7000 }).toBe(true)
        }
        if (scenario === 'delayed-commit') {
          secondHeld = gate.hold('availability')
          held.release()
          phase = 'ordinary cadence starts read two while destination pending'
          await expect.poll(() => {
            gate.check()
            return secondHeld.ready
          }, { timeout: 7000 }).toBe(true)
          assert.equal(secondHeld.status, 200)
        }
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
          await expect(page.getByRole('heading', { name: scenario === 'reload' ? 'Live' : 'Destination', exact: true })).toBeVisible()
          assert.notEqual(await page.evaluate(() => window.readerDocument), documentId)
        }
      }
      held.release()
      secondHeld?.release()
    }

    phase = 'exact read terminal and replacement'
    await expect.poll(async () => {
      const journal = await readJournal()
      const first = journal.filter(event => event.document === documentId && event.read === 1 && terminal(event))
      if (reader === 'intent-fetch' && scenario === 'delayed-commit')
        return journal.some(event => event.document === documentId && event.read === 2 && event.kind === 'native-fetch-rejected' && event.sameAbortReason)
      if (reader === 'intent-fetch' && scenario === 'owner-abort')
        return journal.some(event => event.document === documentId && event.read === 1 && event.kind === 'native-fetch-rejected' && event.sameAbortReason)
      if (reader === 'intent-fetch' && ['download', 'failure-before-intent'].includes(scenario))
        return journal.some(event => event.document === documentId && event.read === 2 && event.kind === 'body')
      if (scenario === 'committed' && journal.some(event => event.read === 1 && event.kind === 'signal-abort' && event.hidden))
        return true
      if (scenario === 'download' && first.some(event => event.kind === 'native-abort'))
        return journal.some(event => event.read === 2 && terminal(event))
      return first.length > 0
    }, { timeout: 7000 }).toBe(true)
    if (scenario === 'failure-before-intent') {
      phase = 'failure remains genuine across later native download intent'
      await expect.poll(() => downloads.length, { timeout: 7000 }).toBe(1)
      assert.equal(downloads[0].suggestedFilename(), 'availability-navigation.html')
      identityRetained = await page.evaluate(() => window.readerDocument) === documentId
      assert.equal(identityRetained, true)
      await expect(page.getByRole('heading', { name: 'Live', exact: true })).toBeVisible()
    }
    if (held) {
      phase = 'positive HTTP close'
      await expect.poll(() => {
        gate.check()
        return [held, ...(secondHeld ? [secondHeld] : [])].every(control => control.closed && ['finished', 'cancelled'].includes(control.terminal))
      }, { timeout: 7000 }).toBe(true)
      httpClosedDuringObservation = true
    }
  }
  catch (error) {
    firstFailure = { phase, error: errorRecord(error) }
  }
  finally {
    held?.release()
    secondHeld?.release()
    destination?.release()
    if (page && !page.isClosed()) {
      try {
        events.push(...await readJournal())
      }
      catch (error) {
        firstFailure ??= { phase: 'journal read', error: errorRecord(error) }
        ownedFailure('journal-read', error)
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

  const originalEvents = events.filter(event => event.document === documentId)
  const first = originalEvents.filter(event => event.read === 1 && terminal(event))
  const trustedNativeCancel = first.some(event => event.kind === 'native-abort' && event.trusted && event.exactTarget && !event.explicitAbort && !event.signalAborted)
  const freshBody = originalEvents.filter(event => event.read === 2 && event.kind === 'body' && event.status === 200 && event.online === true && (reader !== 'intent-fetch' || event.exactBody)).length === 1
  const originalBody = first.filter(event => event.kind === 'body' && event.status === 200 && event.online === true && (reader !== 'intent-fetch' || event.exactBody)).length === 1
  const ownedWithdrawal = events.some(event => event.read === 1 && event.kind === 'signal-abort' && event.hidden && event.signalAborted)
  const unexpectedNativeError = ['success', 'committed', 'download', 'owner-abort'].includes(scenario)
    && events.some(event => ['platform-error', 'platform-timeout'].includes(event.kind) && !event.explicitAbort && !event.signalAborted)
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
  if (reader === 'intent-fetch') {
    const verifiedIntents = originalEvents.filter(event => event.kind === 'navigation-intent' && event.verified && event.trusted && event.exactTarget && event.recognized && event.sameDocument === false)
    const cancellation = first.find(event => event.kind === 'owner-cancelled' && event.reason === 'navigation-intent')
    const withdrawal = originalEvents.find(event => event.kind === 'owner-withdrawal' && event.read === 1 && event.id === cancellation?.intentId)
    const intent = verifiedIntents.find(event => event.id === cancellation?.intentId)
    const signal = originalEvents.find(event => event.read === 1 && event.kind === 'signal-abort' && event.reason === 'navigation-intent' && event.intentId === cancellation?.intentId)
    const nativeRejection = originalEvents.find(event => event.read === 1 && event.kind === 'native-fetch-rejected' && event.sameAbortReason)
    const explicitNativeCancellation = Boolean(intent && withdrawal && signal && nativeRejection && cancellation
      && intent.sequence < withdrawal.sequence && withdrawal.sequence < signal.sequence && signal.sequence < nativeRejection.sequence)
    const resumed = originalEvents.find(event => event.read === 1 && event.kind === 'resume-scheduled')
    const unexpectedFailure = originalEvents.some(event => ['fetch-error', 'body-error', 'http-error', 'error', 'timeout'].includes(event.kind)
      || (['native-fetch-rejected', 'native-body-rejected'].includes(event.kind) && !event.sameAbortReason))
    if (['committed', 'reload', 'delayed-commit'].includes(scenario)) {
      usable = explicitNativeCancellation && first.length === 1 && !unexpectedFailure && !first.some(event => event.kind === 'body')
      if (scenario === 'delayed-commit') {
        const secondStart = originalEvents.find(event => event.read === 2 && event.kind === 'start')
        const pagehide = originalEvents.find(event => event.kind === 'pagehide')
        const secondSignal = originalEvents.find(event => event.read === 2 && event.kind === 'signal-abort' && event.hidden && !event.reason && !event.intentId)
        const secondRejection = originalEvents.find(event => event.read === 2 && event.kind === 'native-fetch-rejected' && event.sameAbortReason)
        const secondTerminal = originalEvents.filter(event => event.read === 2 && terminal(event))
        usable &&= resumed?.delay === 3000 && secondStart && pagehide && secondSignal && secondRejection
          && secondStart.sequence < pagehide.sequence && pagehide.sequence < secondSignal.sequence && secondSignal.sequence < secondRejection.sequence
          && secondTerminal.length === 1 && secondTerminal[0].kind === 'owner-cancelled' && secondTerminal[0].hidden
          && !secondTerminal[0].reason && !secondTerminal[0].intentId
      }
    }
    else if (scenario === 'download') {
      usable = identityRetained && explicitNativeCancellation && first.length === 1 && !first.some(event => event.kind === 'body') && freshBody && !unexpectedFailure && resumed?.delay === 3000
    }
    else if (scenario === 'same-document' || scenario === 'synthetic-intent') {
      usable = identityRetained && originalBody && first.length === 1 && !unexpectedFailure
        && !originalEvents.some(event => event.kind === 'signal-abort' || event.kind === 'owner-withdrawal')
    }
    else if (scenario === 'failure-before-intent') {
      usable = identityRetained && verifiedIntents.length === 1 && first.length === 1 && first[0].kind === 'fetch-error'
        && !originalBody && freshBody && resumed?.delay === 6000 && resumed.failureCount === 1
        && originalEvents.some(event => event.read === 1 && event.kind === 'native-fetch-rejected' && !event.sameAbortReason && event.sequence < verifiedIntents[0].sequence)
    }
    else if (scenario === 'owner-abort')
      usable = first.length === 1 && first[0].kind === 'owner-cancelled' && first[0].signalAborted && !verifiedIntents.length
        && originalEvents.some(event => event.read === 1 && event.kind === 'native-fetch-rejected' && event.sameAbortReason
          && originalEvents.some(signal => signal.read === 1 && signal.kind === 'signal-abort' && signal.sequence < event.sequence))
    else if (scenario === 'http401')
      usable = first.length === 1 && first[0].kind === 'http-error' && first[0].status === 401
  }
  const row = {
    engine,
    reader,
    scenario,
    firstFailure,
    pageErrors,
    navigationKind,
    identityRetained,
    initialDocument: documentId,
    apiRequests,
    usable: usable && !firstFailure && !pageErrors.length && !unexpectedNativeError && (!held || httpClosedDuringObservation),
    httpClosedDuringObservation,
    http: held && {
      ready: held.ready,
      status: held.status,
      closed: held.closed,
      terminal: held.terminal,
    },
    secondHttp: secondHeld && { ready: secondHeld.ready, status: secondHeld.status, closed: secondHeld.closed, terminal: secondHeld.terminal },
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
    for (const reader of ['fetch', 'xhr', 'intent-fetch']) {
      for (const scenario of reader === 'intent-fetch' ? intentScenarios : scenarios)
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

const allPageErrorsStrict = rows.every(row => row.pageErrors.length === 0)
const baselineControlsComplete = rows.filter(row => row.reader === 'fetch').every((row) => {
  if (!['committed', 'download'].includes(row.scenario))
    return row.usable
  if (!row.firstFailure)
    return true
  const original = row.events.filter(event => event.read === 1)
  const pendingOriginal = original.filter(event => event.kind === 'start').length === 1 && !original.some(terminal)
  return row.scenario === 'download'
    && row.identityRetained
    && row.firstFailure.phase === 'exact read terminal and replacement'
    && pendingOriginal
    && row.http?.ready
    && row.http.status === 200
})
const expectedCells = ['chromium', 'webkit'].flatMap(engine => ['fetch', 'xhr', 'intent-fetch'].flatMap(reader => (reader === 'intent-fetch' ? intentScenarios : scenarios).map(scenario => engine + ':' + reader + ':' + scenario))).sort()
const observedCells = rows.map(row => row.engine + ':' + row.reader + ':' + row.scenario).sort()
const allCellsObserved = JSON.stringify(observedCells) === JSON.stringify(expectedCells)
const xhrControlsPassed = rows.filter(row => row.reader !== 'intent-fetch').length === 36
  && allCellsObserved
  && rows.filter(row => row.reader === 'xhr').every(row => row.usable)
  && allPageErrorsStrict
  && baselineControlsComplete
  && cleanupFailures.length === 0
const intentControlsPassed = allCellsObserved && rows.filter(row => row.reader === 'intent-fetch').every(row => row.usable)
  && allPageErrorsStrict && baselineControlsComplete && cleanupFailures.length === 0
await writeEvidence('result.json', {
  diagnosticOnly: true,
  productQualified: false,
  xhrControlsPassed,
  intentControlsPassed,
  allPageErrorsStrict,
  baselineControlsComplete,
  allCellsObserved,
  rows: rows.length,
  cleanupFailures,
})
console.log(JSON.stringify({
  diagnosticOnly: true,
  productQualified: false,
  xhrControlsPassed,
  intentControlsPassed,
  allPageErrorsStrict,
  baselineControlsComplete,
  allCellsObserved,
  rows: rows.length,
  cleanupFailures: cleanupFailures.length,
}))
// Prior genuine XHR failures remain failures, even if the new candidate works.
if (!xhrControlsPassed || !intentControlsPassed)
  process.exitCode = 1
