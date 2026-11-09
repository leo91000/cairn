import type { AvailabilityReadEvent } from './availability-probe'
import { writeFileSync as saveAvailabilityObservation } from 'node:fs'
import { incompleteAvailabilityReads, installAvailabilityProbe } from './availability-probe'
import {
  expect,
  signIn,
  test,
  workspacePath,
} from './fixtures'
import { nativeResponseGate } from './native-response-gate'

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

cacheTest('availability restores the same native document from BFCache with an exact fresh body', async ({ page, workspace, responseGate }, testInfo) => {
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
  let originalDocument: string | undefined
  let heldRead: number | undefined
  let journalAcknowledged = false
  let primaryFailure: unknown
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
    await expect.poll(() => reads.some(event => event.document === original && event.kind === 'body')).toBe(true)
    await harvest()
    const earlierReads = reads.filter(event => event.document === original && event.kind === 'start').length
    poll = responseGate.hold('availability')
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
    const departed = await page.goto(new URL('/health', workspace.url).href)
    expect(departed?.status()).toBe(200)
    poll.release()
    await harvest()
    expect(events(original, target).filter(event => event.kind === 'abort').map(event => ({ trusted: event.trusted, exactTarget: event.exactTarget, insideAbortCall: event.insideAbortCall })))
      .toEqual([{ trusted: true, exactTarget: true, insideAbortCall: true }])
    expect(events(original, target).filter(event => event.kind === 'body')).toEqual([])
    expect(reads.filter(event => event.document === original && event.kind === 'pagehide').map(event => ({ persisted: event.persisted, trusted: event.trusted })))
      .toEqual([{ persisted: true, trusted: true }])
    await expect.poll(() => poll!.closed && ['cancelled', 'finished'].includes(poll!.terminal)).toBe(true)
    await page.goBack()
    expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(original)
    await harvest()
    expect(reads.filter(event => event.document === original && event.kind === 'pageshow' && event.persisted).map(event => event.trusted)).toEqual([true])
    await expect.poll(() => reads.filter(event => event.document === original && event.read > target && event.kind === 'body' && event.exactBody).length).toBeGreaterThan(0)
    await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
    await expect(route).toHaveAttribute('data-transport-route', 'direct')
    // Retire every producer before the final synchronous journal acknowledgement.
    await page.goto(new URL('/health', workspace.url).href)
    await harvest()
    journalAcknowledged = true
  }
  catch (cause) {
    primaryFailure = cause
  }
  finally {
    poll?.release()
    const bounded = async (close: () => Promise<unknown>) => {
      let timer: ReturnType<typeof setTimeout> | undefined
      try {
        await Promise.race([Promise.resolve().then(close), new Promise((_, reject) => timer = setTimeout(() => reject(new Error('BFCache owned cleanup deadline')), 7000))])
      }
      finally {
        clearTimeout(timer)
      }
    }

    const closures = await Promise.allSettled([bounded(() => page.close()), bounded(() => responseGate.close())])
    for (const closure of closures) {
      if (closure.status === 'rejected')
        failures.push(closure.reason)
    }
  }

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
    saveAvailabilityObservation(testInfo.outputPath('availability-bfcache-observation.json'), JSON.stringify({
      journalAcknowledged,
      originalDocument: originalDocument ?? null,
      heldRead: heldRead ?? null,
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

  if (primaryFailure || failures.length)
    throw new AggregateError([...(primaryFailure ? [primaryFailure] : []), ...failures], 'Physical BFCache availability qualification failed')
})
