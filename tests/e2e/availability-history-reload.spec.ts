import type { AvailabilityReadEvent } from './availability-probe'
import { writeFileSync } from 'node:fs'
import { availabilityBodiesAfter, incompleteAvailabilityReads, installAvailabilityProbe } from './availability-probe'
import {
  expect,
  signIn,
  test,
  workspacePath,
} from './fixtures'
import { nativeResponseGate } from './native-response-gate'

const recoveryTest = test.extend<{ responseGate: Awaited<ReturnType<typeof nativeResponseGate>> }>({
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

// Normal supported Playwright profiles: no BFCache reactivation. These are real
// history/reload network loads, with new documents, NOT retained-document restores.
recoveryTest('availability recovers exact fresh documents through authenticated history and reload', async ({ page, workspace, responseGate }, testInfo) => {
  const reads: AvailabilityReadEvent[] = []
  const errors: string[] = []
  const primaryFailures: unknown[] = []
  const failures: unknown[] = []
  const recoveries: Array<{
    mode: 'history' | 'reload'
    departedDocument: string
    freshDocument: string
    heldRead: number
    freshRead: number
    responseStatus: number
    withdrawal: {
      closed: boolean
      terminal: string
      headersDelivered: boolean
      bodyReleased: boolean
    }
  }> = []
  let poll: ReturnType<typeof responseGate.holdBody> | undefined
  let destination: ReturnType<typeof responseGate.hold> | undefined
  let navigation: Promise<unknown> | undefined
  let journalAcknowledged = false
  let gateClosed = false
  const record = (entry: AvailabilityReadEvent) => {
    if (!reads.some(event => event.document === entry.document && event.sequence === entry.sequence))
      reads.push(entry)
  }

  const harvest = async () => {
    for (const event of await page.evaluate(() => JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]))
      record(event)
  }

  page.on('pageerror', error => errors.push(error.message))
  try {
    await workspace.restart()
    await installAvailabilityProbe(page, record)
    await page.goto(workspacePath('/mcps'))
    await signIn(page)
    await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
    const route = page.locator('[data-transport-route]').first()
    await expect(route).toHaveAttribute('data-transport-route', 'direct')
    const expectedAvailability = await page.evaluate(async () => {
      const reference = await fetch('/api/installations', { credentials: 'same-origin' })
      if (!reference.ok)
        throw new Error('Expected authenticated availability reference')
      return reference.json()
    })
    await page.addInitScript(expected => window.cairnExpectedAvailability = expected, expectedAvailability)
    await page.evaluate(expected => window.cairnExpectedAvailability = expected, expectedAvailability)

    for (const mode of ['history', 'reload'] as const) {
      await harvest()
      await expect.poll(() => incompleteAvailabilityReads(reads)).toEqual([])
      const departedDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      const applicationUrl = page.url()
      const earlierReads = reads.filter(event => event.document === departedDocument && event.kind === 'start').length
      poll = responseGate.holdBody()
      await expect.poll(() => {
        responseGate.check()
        return poll!.ready
      }).toBe(true)
      await harvest()
      expect(reads.filter(event => event.document === departedDocument && event.kind === 'start').length).toBe(earlierReads + 1)
      const heldRead = reads.filter(event => event.document === departedDocument && event.kind === 'start').at(-1)!.read
      const events = () => reads.filter(event => event.document === departedDocument && event.read === heldRead)
      expect(poll.status).toBe(200)
      await expect.poll(() => events().filter(event => event.kind === 'body-start').length).toBe(1)
      destination = responseGate.hold(mode === 'history' ? 'health' : 'mcps')
      const arrival = (mode === 'history' ? page.goto(new URL('/health', workspace.url).href) : page.reload())
        .then(response => ({ response }), (error: unknown) => ({ error }))
      navigation = arrival
      await expect.poll(() => destination!.ready).toBe(true)
      expect(destination.status).toBe(200)
      await expect.poll(() => events().filter(event => event.kind === 'abort').map(event => ({ trusted: event.trusted, exactTarget: event.exactTarget, insideAbortCall: event.insideAbortCall })))
        .toEqual([{ trusted: true, exactTarget: true, insideAbortCall: true }])
      await expect.poll(() => events().filter(event => event.kind === 'body-rejected').map(event => event.aborted)).toEqual([true])
      expect(events().filter(event => event.kind === 'body')).toEqual([])
      await expect.poll(() => {
        responseGate.check()
        return poll!.closed && poll!.terminal === 'cancelled' && !poll!.bodyReleased
      }).toBe(true)
      const withdrawal = {
        closed: poll.closed,
        terminal: poll.terminal,
        headersDelivered: poll.headersDelivered,
        bodyReleased: poll.bodyReleased,
      }
      poll.release()
      destination.release()
      const departed = await arrival
      if ('error' in departed)
        throw departed.error
      expect(departed.response?.status()).toBe(200)
      await harvest()
      expect(reads.filter(event => event.document === departedDocument && event.kind === 'pagehide').map(event => event.trusted)).toEqual([true])
      await expect.poll(() => destination!.closed && destination!.terminal === 'finished').toBe(true)
      let response = departed.response
      if (mode === 'history') {
        const returned = page.goBack()
        navigation = returned
        response = await returned
      }

      expect(response?.status()).toBe(200)
      expect(response?.request().isNavigationRequest()).toBe(true)
      expect(page.url()).toBe(applicationUrl)
      await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
      const freshDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      expect(typeof freshDocument).toBe('string')
      expect(freshDocument.length).toBeGreaterThan(0)
      expect(freshDocument).not.toBe(departedDocument)
      await harvest()
      expect(reads.filter(event => event.document === freshDocument && event.kind === 'pageshow').map(event => event.trusted)).toEqual([true])
      await expect.poll(() => availabilityBodiesAfter(reads, freshDocument, 0).length).toBe(1)
      const freshBody = availabilityBodiesAfter(reads, freshDocument, 0)[0]!
      expect(freshBody.status).toBe(200)
      await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
      await expect(route).toHaveAttribute('data-transport-route', 'direct')
      await harvest()
      await expect.poll(() => incompleteAvailabilityReads(reads)).toEqual([])
      recoveries.push({
        mode,
        departedDocument,
        freshDocument,
        heldRead,
        freshRead: freshBody.read,
        responseStatus: response!.status(),
        withdrawal,
      })
    }

    navigation = page.goto(new URL('/health', workspace.url).href)
    await navigation
    await harvest()
    journalAcknowledged = true
  }
  catch (cause) {
    primaryFailures.push(cause)
  }
  finally {
    poll?.release()
    destination?.release()
    const bounded = async (close: () => Promise<unknown>) => {
      let timer: ReturnType<typeof setTimeout> | undefined
      try {
        await Promise.race([Promise.resolve().then(close), new Promise((_, reject) => timer = setTimeout(() => reject(new Error('History/reload owned cleanup deadline')), 7000))])
      }
      finally {
        clearTimeout(timer)
      }
    }

    const closures = await Promise.allSettled([bounded(() => page.close()), bounded(async () => {
      await responseGate.close()
      gateClosed = true
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

  for (const audit of [
    () => expect(incompleteAvailabilityReads(reads)).toEqual([]),
    () => responseGate.check(),
    () => expect(reads.filter(event => ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted)).toEqual([]),
    () => expect(reads.filter(event => event.kind === 'headers' && event.status !== 200)).toEqual([]),
    () => expect(errors).toEqual([]),
    () => expect(recoveries.map(value => value.mode)).toEqual(['history', 'reload']),
    () => expect(journalAcknowledged && page.isClosed() && gateClosed).toBe(true),
  ]) {
    try {
      audit()
    }
    catch (cause) {
      failures.push(cause)
    }
  }

  try {
    writeFileSync(testInfo.outputPath('availability-history-reload.json'), JSON.stringify({
      evidence: 'authenticated-fresh-document-history-reload',
      physicalBFCacheQualified: false,
      nativeGenerationPublicationCorrelation: false,
      recoveries,
      journalAcknowledged,
      pageClosed: page.isClosed(),
      gateClosed,
      reads,
      pageErrors: errors,
      primaryFailureCount: primaryFailures.length,
      auditAndCleanupFailureCount: failures.length,
    }))
  }
  catch (cause) {
    failures.push(cause)
  }

  if (primaryFailures.length || failures.length)
    throw new AggregateError([...primaryFailures, ...failures], 'Authenticated fresh-document history/reload recovery failed')
})
