import type { AvailabilityReadEvent } from './availability-probe'
import { writeFileSync } from 'node:fs'
import { test as base, expect } from '@playwright/test'
import {
  availabilityLifecycleFixture,
  lifecycleAvailability,
  lifecycleBound,
  retireLifecycleOwners,
} from './availability-lifecycle-fixture'
import { incompleteAvailabilityReads, installAvailabilityProbe } from './availability-probe'
import { nativeResponseGate } from './native-response-gate'

const test = base.extend<{
  lifecycle: Awaited<ReturnType<typeof availabilityLifecycleFixture>>
  responseGate: Awaited<ReturnType<typeof nativeResponseGate>>
}>({
  // eslint-disable-next-line no-empty-pattern
  lifecycle: async ({}, use) => {
    const fixture = await availabilityLifecycleFixture()
    try {
      await use(fixture)
    }
    finally {
      await lifecycleBound(() => fixture.close(), 'HTTP fixture teardown')
    }
  },
  responseGate: async ({ lifecycle }, use) => {
    const gate = await nativeResponseGate(lifecycle.url)
    try {
      await use(gate)
    }
    finally {
      await lifecycleBound(() => gate.close(), 'response gate teardown')
    }
  },
  proxy: async ({ responseGate }, use) => use({ server: responseGate.server }),
})

test.use({ launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] } })

test('restores the real availability caller in an eligible synthetic lifecycle fixture', async ({ page, lifecycle, responseGate }, testInfo) => {
  const reads: AvailabilityReadEvent[] = []
  const errors: string[] = []
  const withdrawals: unknown[] = []
  const failures: unknown[] = []
  const record = (entry: AvailabilityReadEvent) => {
    if (!reads.some(event => event.document === entry.document && event.sequence === entry.sequence))
      reads.push(entry)
  }

  const harvest = async () => {
    for (const event of await page.evaluate(() => JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]))
      record(event)
  }

  const lifecycleEvents = (id: string, kind: 'pagehide' | 'pageshow') => reads.filter(event => event.document === id && event.kind === kind)
  let poll: ReturnType<typeof responseGate.holdBody> | undefined
  let destination: ReturnType<typeof responseGate.hold> | undefined
  let navigation: Promise<unknown> | undefined
  let original: string | undefined
  let inertRestored = false
  let completedCycles = 0
  const primaryFailures: unknown[] = []
  let journalAcknowledged = false
  page.on('pageerror', error => errors.push(error.message))
  await installAvailabilityProbe(page, record)
  try {
    // Positive native support control in the SAME server/profile; not app proof.
    await page.goto(`${lifecycle.url}/inert`)
    const inert = await page.evaluate(() => window.cairnAvailabilityDocument)
    await page.goto(`${lifecycle.url}/health`)
    await harvest()
    expect(lifecycleEvents(inert, 'pagehide').map(event => ({ persisted: event.persisted, trusted: event.trusted })))
      .toEqual([{ persisted: true, trusted: true }])
    await page.goBack()
    expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(inert)
    await harvest()
    expect(lifecycleEvents(inert, 'pageshow').filter(event => event.persisted).map(event => event.trusted)).toEqual([true])
    inertRestored = true

    await page.goto(`${lifecycle.url}/claim`)
    await expect(page.getByRole('button', { name: 'Lifecycle installation · Online', exact: true })).toBeVisible()
    original = await page.evaluate(() => window.cairnAvailabilityDocument)
    await expect.poll(() => reads.some(event => event.document === original && event.kind === 'body')).toBe(true)
    for (const online of [false, true]) {
      await harvest()
      const previousStarts = reads.filter(event => event.document === original && event.kind === 'start').length
      poll = responseGate.holdBody()
      await expect.poll(() => {
        responseGate.check()
        return poll!.ready
      }).toBe(true)
      await harvest()
      expect(reads.filter(event => event.document === original && event.kind === 'start').length).toBe(previousStarts + 1)
      const target = reads.filter(event => event.document === original && event.kind === 'start').at(-1)!.read
      const events = () => reads.filter(event => event.document === original && event.read === target)
      expect(poll.status).toBe(200)
      await expect.poll(() => events().filter(event => event.kind === 'body-start').length).toBe(1)
      await page.evaluate(expected => window.cairnExpectedAvailability = expected, lifecycleAvailability(online))
      destination = responseGate.hold('health')
      const arrival = page.goto(`${lifecycle.url}/health`).then(response => ({ response }), (error: unknown) => ({ error }))
      navigation = arrival
      await expect.poll(() => destination!.ready).toBe(true)
      expect(destination.status).toBe(200)
      await expect.poll(() => events().filter(event => event.kind === 'abort').map(event => ({ trusted: event.trusted, exactTarget: event.exactTarget, insideAbortCall: event.insideAbortCall })))
        .toEqual([{ trusted: true, exactTarget: true, insideAbortCall: true }])
      await expect.poll(() => events().filter(event => event.kind === 'body-rejected').map(event => event.aborted)).toEqual([true])
      expect(events().filter(event => event.kind === 'body')).toEqual([])
      await expect.poll(() => poll!.closed && poll!.terminal === 'cancelled' && !poll!.bodyReleased).toBe(true)
      withdrawals.push({ closed: poll.closed, terminal: poll.terminal, bodyReleased: poll.bodyReleased })
      poll.release()
      destination.release()
      const departed = await arrival
      if ('error' in departed)
        throw departed.error
      expect(departed.response?.status()).toBe(200)
      await harvest()
      expect(lifecycleEvents(original, 'pagehide').map(event => ({ persisted: event.persisted, trusted: event.trusted })))
        .toEqual(Array.from({ length: completedCycles + 1 }, () => ({ persisted: true, trusted: true })))
      lifecycle.setAvailability(online)
      const requestsBeforeReturn = responseGate.availabilityRequests
      navigation = page.goBack()
      await navigation
      expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(original)
      await harvest()
      const restored = lifecycleEvents(original, 'pageshow').filter(event => event.persisted)
      expect(restored.map(event => event.trusted)).toEqual(Array.from({ length: completedCycles + 1 }, () => true))
      const restoredSequence = restored.at(-1)!.sequence
      await expect.poll(() => reads.filter(event => event.document === original && event.kind === 'body' && event.sequence > restoredSequence).map(event => ({ status: event.status, exactBody: event.exactBody })))
        .toEqual([{ status: 200, exactBody: true }])
      const freshBody = reads.find(event => event.document === original && event.kind === 'body' && event.sequence > restoredSequence)!
      const freshStart = reads.find(event => event.document === original && event.kind === 'start' && event.read === freshBody.read)!
      expect(freshStart.sequence).toBeGreaterThan(restoredSequence)
      expect(responseGate.availabilityRequests).toBe(requestsBeforeReturn + 1)
      await expect(page.getByRole('button', { name: `Lifecycle installation · ${online ? 'Online' : 'Offline'}`, exact: true })).toBeVisible()
      completedCycles++
    }
    navigation = page.goto(`${lifecycle.url}/health`)
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
    failures.push(...await retireLifecycleOwners([
      async () => {
        await page.close()
        return page.isClosed()
      },
      async () => {
        await responseGate.close()
        return true
      },
      async () => {
        await lifecycle.close()
        return true
      },
    ]))
    if (navigation) {
      try {
        await lifecycleBound(() => navigation!, 'navigation drain')
      }
      catch (cause) {
        failures.push(cause)
      }
    }
  }
  const audits = [
    () => expect(incompleteAvailabilityReads(reads)).toEqual([]),
    () => expect(reads.filter(event => ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted)).toEqual([]),
    () => expect(reads.filter(event => event.kind === 'headers' && event.status !== 200)).toEqual([]),
    () => expect(errors).toEqual([]),
    () => responseGate.check(),
    () => lifecycle.check(),
    () => expect(inertRestored && completedCycles === 2 && journalAcknowledged).toBe(true),
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
    writeFileSync(testInfo.outputPath('availability-lifecycle.json'), JSON.stringify({
      scope: 'synthetic-claim-no-selected-installation',
      appDirectQualified: false,
      nativeGenerationPublicationCorrelation: false,
      inertRestored,
      completedCycles,
      journalAcknowledged,
      withdrawals,
      reads,
      pageErrors: errors,
      failureCount: failures.length + primaryFailures.length,
    }))
  }
  catch (cause) {
    failures.push(cause)
  }
  if (primaryFailures.length || failures.length)
    throw new AggregateError([...primaryFailures, ...failures], 'Synthetic lifecycle restoration failed')
})
