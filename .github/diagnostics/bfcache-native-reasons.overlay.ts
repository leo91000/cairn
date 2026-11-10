import type { AvailabilityReadEvent } from './availability-probe'
import { writeFileSync as saveAvailabilityObservation } from 'node:fs'
import { availabilityBodiesAfter, incompleteAvailabilityReads, installAvailabilityProbe } from './availability-probe'
import {
  expect,
  signIn,
  test,
  workspacePath,
} from './fixtures'
import { nativeResponseGate } from './native-response-gate'
import { acquireNativeReasonDiagnostic, armNativeHistoryIdentity, readNativeHistoryReasons, finalizeNativeReasonDiagnostic } from '../../test-results/native-bfcache-reasons/bfcache-native-reasons.mjs'

const cacheTest = test.extend<{ responseGate: Awaited<ReturnType<typeof nativeResponseGate>> }>({
  responseGate: async ({ workspace }, use) => {
    const gate = await nativeResponseGate(workspace.url)
    try {
      await use(gate)
    }
    finally {
      await gate.close()
    }
  },
  proxy: async ({ responseGate }, use) => use({ server: responseGate.server }),
})

// Only Chromium has this default disabling switch; filtering it is inert for WebKit.
cacheTest.use({ launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } })

cacheTest('availability native not-restored reasons diagnostic only', async ({ page, workspace, responseGate, browser }, testInfo) => {
  const reads: AvailabilityReadEvent[] = []
  const errors: string[] = []
  const record = (entry: AvailabilityReadEvent) => {
    if (!reads.some(event => event.document === entry.document && event.sequence === entry.sequence))
      reads.push(entry)
  }

  const harvest = async () => {
    for (const event of await page.evaluate(() => JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]))
      record(event)
  }

  const events = (document: string, read: number) => reads.filter(event => event.document === document && event.read === read)
  page.on('pageerror', error => errors.push(error.message))
  await workspace.restart()
  await installAvailabilityProbe(page, record)
  let poll: ReturnType<typeof responseGate.hold> | undefined
  let destination: ReturnType<typeof responseGate.hold> | undefined
  let navigation: Promise<unknown> | undefined
  let withdrawalReceipt: {
    headersDelivered: boolean
    bodyReleased: boolean
    closed: boolean
    terminal: string
  } | undefined
  let originalDocument: string | undefined
  let heldRead: number | undefined
  let restorationSequence: number | undefined
  let restoredRead: number | undefined
  let journalAcknowledged = false
  let primaryFailure: unknown
  let nativeReasonReceipt: any
  let nativeArmReceipt: any
  const diagnosticFailures: unknown[] = []
  let cleanupFailureCount = 0
  let httpClosed = false
  const failures: unknown[] = []
  try {
    await page.goto(workspacePath('/mcps'))
    await signIn(page)
    await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
    const route = page.locator('[data-transport-route]').first()
    await expect(route).toHaveAttribute('data-transport-route', 'direct')
    await page.evaluate(async () => {
      const reference = await fetch('/api/installations', { credentials: 'same-origin' })
      if (!reference.ok)
        throw new Error('Expected authenticated availability reference')
      window.cairnExpectedAvailability = await reference.json()
    })
    const original = await page.evaluate(() => window.cairnAvailabilityDocument)
    originalDocument = original
    const identity = await page.evaluate(armNativeHistoryIdentity)
    nativeArmReceipt = identity
    if (!identity.armed)
      throw new Error('Native history identity admission is inconclusive')
    await expect.poll(() => reads.some(event => event.document === original && event.kind === 'body')).toBe(true)
    await harvest()
    const earlierReads = reads.filter(event => event.document === original && event.kind === 'start').length
    poll = responseGate.holdBody()
    await expect.poll(() => {
      responseGate.check()
      return poll!.ready
    }).toBe(true)
    // A binding may lag the server's ready receipt. Read the synchronous journal
    // before assigning the held request's identity; never select an older read.
    await harvest()
    expect(reads.filter(event => event.document === original && event.kind === 'start').length).toBeGreaterThan(earlierReads)
    const target = reads.filter(event => event.document === original && event.kind === 'start').at(-1)!.read
    heldRead = target
    expect(poll.status).toBe(200)
    await expect.poll(() => events(original, target).filter(event => event.kind === 'body-start').length).toBe(1)
    destination = responseGate.hold('health')
    const arrival = page.goto(new URL('/health', workspace.url).href).then(response => ({ response }), (error: unknown) => ({ error }))
    navigation = arrival
    await expect.poll(() => destination!.ready).toBe(true)
    expect(destination.status).toBe(200)
    await expect.poll(() => events(original, target).filter(event => event.kind === 'abort').map(event => ({ trusted: event.trusted, exactTarget: event.exactTarget, insideAbortCall: event.insideAbortCall })))
      .toEqual([{ trusted: true, exactTarget: true, insideAbortCall: true }])
    await expect.poll(() => events(original, target).filter(event => event.kind === 'body-rejected').map(event => event.aborted)).toEqual([true])
    expect(events(original, target).filter(event => event.kind === 'body')).toEqual([])
    await expect.poll(() => {
      responseGate.check()
      return poll!.closed && poll!.terminal === 'cancelled' && !poll!.bodyReleased
    }).toBe(true)
    withdrawalReceipt = {
      headersDelivered: poll.headersDelivered,
      bodyReleased: poll.bodyReleased,
      closed: poll.closed,
      terminal: poll.terminal,
    }
    poll.release()
    destination.release()
    const departed = await arrival
    if ('error' in departed)
      throw departed.error
    expect(departed.response?.status()).toBe(200)
    await harvest()
    const outcome = await acquireNativeReasonDiagnostic({
      checkpoint: async () => {
        expect(reads.filter(event => event.document === original && event.kind === 'pagehide').map(event => ({ persisted: event.persisted, trusted: event.trusted })))
          .toEqual([{ persisted: true, trusted: true }])
      },
      checkpointFlags: () => reads.filter(event => event.document === original && event.kind === 'pagehide').map(event => ({ persisted: event.persisted, trusted: event.trusted })),
      browserVersion: () => browser.version(),
      history: () => {
        const firstHistory = page.goBack()
        navigation = firstHistory
        return firstHistory
      },
      harvest,
      observe: () => page.evaluate(readNativeHistoryReasons),
    })
    nativeReasonReceipt = outcome.receipt
    diagnosticFailures.push(...outcome.failures)
    expect(poll.terminal).toBe('cancelled')
    // Preserve every checkpoint failure before any later settling can fail.
    if (outcome.receipt.historyCompleted) {
      // These existing waits settle the current document; they never qualify
      // restoration of the original document after a cache miss.
      await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
      await expect(route).toHaveAttribute('data-transport-route', 'direct')
      // Existing retirement and synchronous journal acknowledgement are retained.
      await page.goto(new URL('/health', workspace.url).href)
      await harvest()
      journalAcknowledged = true
    }
  }
  catch (cause) {
    primaryFailure = cause
  }
  finally {
    poll?.release()
    destination?.release()
    const bounded = async (close: () => Promise<unknown>) => {
      let timer: ReturnType<typeof setTimeout> | undefined
      try {
        await Promise.race([Promise.resolve().then(close), new Promise((_, reject) => timer = setTimeout(() => reject(new Error('BFCache owned cleanup deadline')), 7000))])
      }
      finally {
        clearTimeout(timer)
      }
    }

    const closures = await Promise.allSettled([bounded(() => page.close()), bounded(async () => {
      await responseGate.close()
      httpClosed = true
    })])
    for (const closure of closures) {
      if (closure.status === 'rejected')
        failures.push(closure.reason)
    }

    if (navigation) {
      try {
        await bounded(() => navigation!)
      }
      catch (cause) {
        failures.push(cause)
      }
    }
  }

  cleanupFailureCount = failures.length

  // Audit independently so simultaneous native/HTTP/page errors all survive.
  const audits = [
    () => expect(incompleteAvailabilityReads(reads)).toEqual([]),
    () => responseGate.check(),
    () => expect(reads.filter(event => ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted)).toEqual([]),
    () => expect(reads.filter(event => event.kind === 'headers' && event.status !== 200)).toEqual([]),
    () => expect(errors).toEqual([]),
  ]
  for (const audit of audits) {
    try {
      audit()
    }
    catch (cause) {
      failures.push(cause)
    }
  }

  try {
    const nativeFinal = finalizeNativeReasonDiagnostic(nativeReasonReceipt, {
      arm: nativeArmReceipt,
      cleanupFailureCount,
      auditFailureCount: failures.length - cleanupFailureCount,
      pageClosed: page.isClosed(),
      httpClosed,
      journalAcknowledged,
    })
    saveAvailabilityObservation(testInfo.outputPath('native-bfcache-reasons.json'), JSON.stringify(nativeFinal))
    if (!nativeFinal.observationComplete)
      failures.push(new Error('Native BF reason observation or closure is inconclusive'))
  }
  catch (cause) {
    failures.push(cause)
  }

  try {
    saveAvailabilityObservation(testInfo.outputPath('availability-bfcache-observation.json'), JSON.stringify({
      journalAcknowledged,
      originalDocument: originalDocument ?? null,
      heldRead: heldRead ?? null,
      restorationSequence: restorationSequence ?? null,
      restoredRead: restoredRead ?? null,
      withdrawalReceipt: withdrawalReceipt ?? null,
      destination: destination
        ? {
            received: destination.received,
            ready: destination.ready,
            status: destination.status ?? null,
            terminal: destination.terminal,
            closed: destination.closed,
          }
        : null,
      reads,
      pageErrors: errors,
      poll: poll
        ? {
            received: poll.received,
            ready: poll.ready,
            status: poll.status ?? null,
            terminal: poll.terminal,
            closed: poll.closed,
          }
        : null,
    }))
  }
  catch (cause) {
    failures.push(cause)
  }

  if (primaryFailure || diagnosticFailures.length || failures.length)
    throw new AggregateError([...diagnosticFailures, ...(primaryFailure ? [primaryFailure] : []), ...failures], 'Physical BFCache availability qualification failed')
})
