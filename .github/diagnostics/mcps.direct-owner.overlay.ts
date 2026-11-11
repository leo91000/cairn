import type { Download } from '@playwright/test'
import type { AvailabilityReadEvent } from './availability-probe'
import { writeFileSync as saveAvailabilityObservation } from 'node:fs'
import { mcpProvider } from '../fixtures/mcp-provider'
import { availabilityBodiesAfter, incompleteAvailabilityReads, installAvailabilityProbe } from './availability-probe'
import {
  signIn as authenticateWorkspace,
  createAgent,
  expect,
  expectSingleScroll,
  openAgentSection,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import { nativeResponseGate } from './native-response-gate'

type OAuthDepartureProbe = { kind: 'background' } | { kind: 'offer', closed: boolean } | { kind: 'observer-error' }

declare global {
  interface Window {
    cairnOAuthProbe: (probe: OAuthDepartureProbe) => Promise<void>
  }
}

test('failed saves keep feedback visible above a scrolled MCP form', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.setViewportSize({ width: 390, height: 440 })
  await page.goto(workspacePath('/mcps'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Add MCP', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByRole('combobox', { name: 'Connection type', exact: true }).click()
  await page.getByRole('option', { name: 'Command in agent VM', exact: true }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('Preserved MCP')
  await dialog.getByLabel('Command', { exact: true }).fill('printf')
  for (let index = 1; index <= 8; index++) {
    await dialog.getByRole('button', { name: 'Add variable', exact: true }).click()
    await dialog.getByLabel(`Variable ${index} name`).fill(`FIXTURE_${index}`)
    await dialog.getByLabel(`Variable ${index} value`).fill('fixture')
  }

  let saves = 0
  await page.route(/\/mcps$/, async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    saves++
    await route.fulfill({ status: 503, json: { error: 'Installation unavailable' } })
  })
  await page.route(/\/overview$/, route => route.fulfill({ status: 503, json: { error: 'Installation unavailable' } }))
  await dialog.getByRole('button', { name: 'Save MCP', exact: true }).click()
  const feedback = dialog.getByRole('status').filter({ hasText: /Connection unavailable|Connection lost/ })
  await expect(feedback).toBeInViewport({ ratio: 1 })
  expect(await dialog.locator('.modal-inner').evaluate(element => element.scrollTop)).toBeGreaterThan(0)
  await expect(dialog.getByLabel('Name', { exact: true })).toHaveValue('Preserved MCP')
  expect(saves).toBe(1)
})

for (const pending of ['authorization', 'offer'] as const) {
  test(`OAuth navigation does not leave background polls running in the departing page (${pending} pending)`, async ({ page, workspace, browserName }) => {
    const name = `Slow OAuth (${pending})`
    let backgroundRequests = 0
    let observationFailures = 0
    const closedOffers: boolean[] = []
    const offerCompleted = Promise.withResolvers<void>()
    await page.exposeFunction('cairnOAuthProbe', (probe: OAuthDepartureProbe) => {
      if (probe.kind === 'observer-error') {
        observationFailures++
        return
      }

      if (probe.kind === 'background') {
        backgroundRequests++
        return
      }

      closedOffers.push(probe.closed)
      offerCompleted.resolve()
    })
    await page.clock.install()
    await page.addInitScript((holdOffer) => {
      'use strict'

      let departing = false
      const request = window.fetch
      const enqueueDiagnosticError = window.queueMicrotask
      if (typeof enqueueDiagnosticError !== 'function')
        throw new Error('OAuth probe requires native diagnostic scheduling')
      const signalAborted = Object.getOwnPropertyDescriptor(AbortSignal.prototype, 'aborted')?.get
      const listenForAbort = EventTarget.prototype.addEventListener
      if (!signalAborted)
        throw new Error('OAuth departure probe requires native AbortSignal state')

      const reportObservationFailure = () => {
        try {
          void window.cairnOAuthProbe({ kind: 'observer-error' })
        }
        catch (reportingFailure) {
          Reflect.apply(enqueueDiagnosticError, window, [() => {
            throw reportingFailure
          }])
        }
      }

      const emitProbe = (probe: OAuthDepartureProbe) => {
        try {
          void window.cairnOAuthProbe(probe)
        }
        catch {
          reportObservationFailure()
        }
      }

      window.fetch = function (this: unknown, ...args: Parameters<typeof request>) {
        const result = Reflect.apply(request, this, args)
        try {
          const path = args[0]
          if (typeof path === 'string' && path.endsWith('/direct/authorize')) {
            const options = args[1]
            const descriptor = options && typeof options === 'object' ? Object.getOwnPropertyDescriptor(options, 'signal') : undefined
            const signal = descriptor && 'value' in descriptor ? descriptor.value : undefined
            if (signal instanceof AbortSignal) {
              if (Reflect.apply(signalAborted, signal, [])) {
                departing = true
              }
              else {
                Reflect.apply(listenForAbort, signal, ['abort', () => {
                  departing = true
                }, { once: true }])
              }
            }
          }

          const background = typeof path === 'string' && (path === '/api/installations' || path.endsWith('/tasks/activity') || path.endsWith('/direct/authorize'))
          if (background && departing)
            emitProbe({ kind: 'background' })
        }
        catch {
          reportObservationFailure()
        }

        return result
      }

      const availability = new WeakSet<XMLHttpRequest>()
      const open = XMLHttpRequest.prototype.open
      const send = XMLHttpRequest.prototype.send
      XMLHttpRequest.prototype.open = function (this: XMLHttpRequest, ...args: unknown[]) {
        const result = Reflect.apply(open, this, args)

        try {
          availability.delete(this)
          if (args[0] === 'GET' && args[1] === '/api/installations')
            availability.add(this)
        }
        catch {
          reportObservationFailure()
        }

        return result
      }

      XMLHttpRequest.prototype.send = function (this: XMLHttpRequest, ...args: unknown[]) {
        const result = Reflect.apply(send, this, args)

        try {
          if (availability.has(this) && departing)
            emitProbe({ kind: 'background' })
        }
        catch {
          reportObservationFailure()
        }

        return result
      }

      if (!holdOffer || !location.pathname.endsWith('/mcps'))
        return

      const heldOffers = new WeakMap<RTCPeerConnection, () => void>()
      const close = RTCPeerConnection.prototype.close
      Object.defineProperty(RTCPeerConnection.prototype, 'close', {
        value(this: RTCPeerConnection) {
          Reflect.apply(close, this, [])
          const release = heldOffers.get(this)
          if (!release)
            return
          heldOffers.delete(this)
          departing = true
          release()
        },
      })
      const createOffer = RTCPeerConnection.prototype.createOffer
      Object.defineProperty(RTCPeerConnection.prototype, 'createOffer', {
        async value(this: RTCPeerConnection, options?: RTCOfferOptions) {
          const offer = await Reflect.apply(createOffer, this, [options])
          const peerClosed = () => this.connectionState === 'closed'
          if (peerClosed())
            return offer

          const released = new Promise<void>(resolve => heldOffers.set(this, resolve))
          document.documentElement.dataset.offerReady = 'true'
          await released
          emitProbe({ kind: 'offer', closed: peerClosed() })
          return offer
        },
      })
    }, pending === 'offer')

    const authorizing = Promise.withResolvers<void>()
    const authorized = Promise.withResolvers<void>()
    const provider = await mcpProvider({
      beforeAuthorize: async () => {
        authorizing.resolve()
        await authorized.promise
      },
    })
    const errors: string[] = []
    let directCancelled = false
    let directCancellationUrl: string | undefined
    let directCancellationReason: string | undefined
    page.on('pageerror', error => errors.push(error.message))
    try {
      await workspace.api('/api/mcps', 'POST', {
        name,
        transport: 'http',
        url: `${provider.origin}/mcp`,
        auth: 'oauth',
        allowPrivateNetwork: true,
      })

      const direct = Promise.withResolvers<void>()
      const directCompleted = Promise.withResolvers<void>()
      page.on('requestfailed', (request) => {
        if (request.url().endsWith('/direct/authorize')) {
          directCancelled = true
          directCancellationUrl = request.url()
          directCancellationReason = request.failure()?.errorText
        }
      })
      if (pending === 'authorization') {
        await page.route('**/direct/authorize', async (route) => {
          const response = await route.fetch()
          direct.resolve()
          await authorizing.promise
          // The departing transport has canceled this held authorization.
          await route.fulfill({ response }).catch(() => {})
          directCompleted.resolve()
        })
      }

      await page.goto(workspacePath('/mcps'))
      await authenticateWorkspace(page)
      if (pending === 'offer')
        await page.waitForFunction(() => document.documentElement.dataset.offerReady === 'true')
      else
        await direct.promise

      const card = page.locator('.mcp-card').filter({ hasText: name })
      const connecting = card.getByRole('button', { name: 'Connect', exact: true }).click()
      await authorizing.promise
      if (pending === 'authorization')
        await directCompleted.promise
      else
        await offerCompleted.promise

      // Closing the real peer releases its deferred native offer before navigation
      // commits. WebKit also lets us advance the departing page across the initial
      // 3s and recurring 30s shell polls; Chromium holds evaluations until commit.
      if (browserName === 'webkit')
        await page.clock.runFor(31000)

      authorized.resolve()
      await connecting
      await page.waitForURL(url => url.pathname.endsWith('/mcps/callback'))
      await expect(card.getByText('Connected', { exact: true })).toBeVisible()
      expect(provider.exchanges).toBe(1)
      expect(backgroundRequests).toBe(0)
      expect(observationFailures).toBe(0)
      if (pending === 'offer')
        expect(closedOffers).toEqual([true])
      else
        expect(directCancelled).toBe(true)
      // WebKit reports this held, cancelled fetch as an access-control page error.
      const expectedWebKitCancellation = browserName === 'webkit'
        && pending === 'authorization'
        && directCancellationReason === 'Load request cancelled'
      const unexpectedErrors = [...errors]
      if (expectedWebKitCancellation && directCancellationUrl) {
        const cancelledUrl = new URL(directCancellationUrl)
        const cancellationError = `/${cancelledUrl.host}${cancelledUrl.pathname} due to access control checks.`
        const cancellationErrorIndex = unexpectedErrors.findIndex(error => error === cancellationError)
        if (cancellationErrorIndex !== -1)
          unexpectedErrors.splice(cancellationErrorIndex, 1)
      }

      expect(unexpectedErrors).toEqual([])
    }
    finally {
      authorizing.resolve()
      authorized.resolve()
      try {
        await page.unrouteAll({ behavior: 'wait' })
      }
      finally {
        await provider.close()
      }
    }
  })
}

test('manages MCP connections, OAuth consent, tools and agent access on desktop and mobile', async ({ page, workspace }, testInfo) => {
  const provider = await mcpProvider()
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  try {
    await page.goto(workspacePath('/mcps'))
    await authenticateWorkspace(page)
    await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Add MCP', exact: true }).click()
    const dialog = page.getByRole('dialog')
    await dialog.getByLabel('Name', { exact: true }).fill('Design workspace')
    await dialog.getByLabel('Server URL', { exact: true }).fill(`${provider.origin}/mcp`)
    await dialog.getByRole('combobox', { name: 'Authentication', exact: true }).click()
    await page.getByRole('option', { name: 'OAuth · sign in with your account', exact: true }).click()
    await dialog.getByLabel('Allow private network endpoints').check()
    for (const theme of ['light', 'dark'] as const) {
      await page.emulateMedia({ colorScheme: theme })
      for (const width of [1440, 320, 390]) {
        await page.setViewportSize({ width, height: width === 1440 ? 1000 : 844 })
        await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
        await expectSingleScroll(page)
        await page.screenshot({ path: testInfo.outputPath(`${theme}-${width}-oauth-editor.png`), animations: 'disabled' })
      }
    }

    await dialog.getByRole('button', { name: 'Save MCP' }).click()
    const card = page.locator('.mcp-card').filter({ hasText: 'Design workspace' })
    await card.getByRole('button', { name: 'Connect', exact: true }).click()
    await expect(card.getByText('Connected', { exact: true })).toBeVisible()
    expect(provider.exchanges).toBe(1)
    await card.getByRole('button', { name: /Browse tools/ }).click()
    await dialog.getByLabel('Enable all tools, including future tools').uncheck()
    await dialog.locator('.mcp-tool').filter({ hasText: 'Echo message' }).getByRole('checkbox').check()
    await dialog.getByRole('button', { name: 'Save tool access' }).click()
    await card.getByRole('button', { name: 'Test', exact: true }).click()
    await expect(card.getByText('Connected', { exact: true })).toBeVisible()
    for (const theme of ['light', 'dark'] as const) {
      await page.emulateMedia({ colorScheme: theme })
      for (const width of [1440, 320, 390]) {
        await page.setViewportSize({ width, height: width === 1440 ? 1000 : 844 })
        await page.screenshot({ path: testInfo.outputPath(`${theme}-${width}-connections.png`), animations: 'disabled' })
      }
    }

    await card.getByRole('button', { name: /Browse tools/ }).click()
    await page.screenshot({ path: testInfo.outputPath('dark-mobile-tools.png'), animations: 'disabled' })
    await dialog.getByRole('button', { name: 'Cancel', exact: true }).click()
    await page.goto(workspacePath('/agents'))
    await page.getByRole('link', { name: 'New agent', exact: true }).click()
    await page.getByLabel('Name', { exact: true }).fill('MCP reader')
    await openAgentSection(page, 'MCP & tools')
    await page.getByRole('group', { name: 'MCP connections access' }).getByRole('button', { name: 'Choose', exact: true }).click()
    await page.getByLabel('Design workspace', { exact: true }).check()
    await page.locator('summary').filter({ hasText: 'Tools ·' }).click()
    await page.getByLabel('All enabled tools', { exact: true }).uncheck()
    await page.getByLabel('echo', { exact: true }).check()
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath('dark-mobile-agent-access.png'), animations: 'disabled' })
    const id = await createAgent(page)
    const reader = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === id)
    expect(reader.access.mcpTools).toEqual({ [reader.access.mcps[0]]: ['echo'] })
    await page.goto(workspacePath('/mcps'))
    await page.getByRole('button', { name: 'Add MCP', exact: true }).click()
    await dialog.getByRole('combobox', { name: 'Connection type', exact: true }).click()
    await page.getByRole('option', { name: 'Command in agent VM', exact: true }).click()
    await dialog.getByLabel('Name', { exact: true }).fill('Local tooling')
    await dialog.getByLabel('Command', { exact: true }).fill('pnpm')
    await dialog.getByLabel('Arguments', { exact: true }).fill('dlx\nexample-mcp-server')
    await dialog.getByRole('button', { name: 'Add variable' }).click()
    await dialog.getByLabel('Variable 1 name').fill('API_TOKEN')
    await dialog.getByLabel('Variable 1 value').fill('synthetic-browser-test-token')
    await page.screenshot({ path: testInfo.outputPath('dark-mobile-command-editor.png'), animations: 'disabled' })
    await dialog.getByRole('button', { name: 'Save MCP' }).click()
    await expect(page.locator('.mcp-card').filter({ hasText: 'Local tooling' })).toBeVisible()
    await card.getByLabel('Actions for Design workspace').click()
    await card.getByRole('button', { name: 'Remove credentials', exact: true }).click()
    await dialog.getByRole('button', { name: 'Remove credentials', exact: true }).click()
    await expect(card.getByText('Not tested', { exact: true })).toBeVisible()
    expect(errors).toEqual([])
  }
  finally { await provider.close() }
})

const availabilityTest = test.extend<{ responseGate: Awaited<ReturnType<typeof nativeResponseGate>> }>({
  responseGate: async ({ workspace }, use) => {
    const gate = await nativeResponseGate(workspace.url)
    try {
      await use(gate)
    }
    finally {
      await gate.close()
    }
  },
  proxy: async ({ responseGate }, use) => {
    await use({ server: responseGate.server })
  },
})

for (const destination of ['committed', 'cancelled', 'restored'] as const) {
  const title = destination === 'restored'
    ? 'availability resumes real reads after same-document persisted lifecycle'
    : `availability keeps direct readers stable until native navigation is ${destination}`

  availabilityTest(title, async ({ page, workspace, responseGate }, testInfo) => {
    const errors: string[] = []
    const reads: AvailabilityReadEvent[] = []
    let availabilityProbeFailures = 0
    const downloads: Download[] = []
    const withdrawalReceipts: Array<{
      label: string
      document: string
      read: number
      headersDelivered: boolean
      bodyReleased: boolean
      terminal: string
      closed: boolean
    }> = []
    const recordRead = (event: AvailabilityReadEvent) => {
      if (!reads.some(record => record.document === event.document && record.sequence === event.sequence))
        reads.push(event)
    }

    page.on('download', download => downloads.push(download))
    let stage = 'fixture reset'
    let primaryFailure: unknown
    const cleanupFailures: unknown[] = []
    let poll: ReturnType<typeof responseGate.hold> | undefined
    let secondPoll: ReturnType<typeof responseGate.hold> | undefined
    let secondRead: number | undefined
    let retainedSequence: number | undefined
    let firstRead: number | undefined
    let departingDocumentId: string | undefined
    let journalAcknowledged = false
    let navigationResponse: ReturnType<typeof responseGate.hold> | undefined
    let navigation: Promise<unknown> | undefined
    let navigationValidated = false
    const gates: Array<{
      label: string
      document: string | null
      read: number | null
      response: ReturnType<typeof responseGate.hold>
    }> = []
    const observeGate = (label: string, response: ReturnType<typeof responseGate.hold>) => {
      const gate = {
        label,
        document: departingDocumentId ?? null,
        read: null as number | null,
        response,
      }
      gates.push(gate)
      return gate
    }

    page.on('pageerror', error => errors.push(error.message))
    // Accepted grants belong to this worker's earlier pages until expiry. Use
    // the existing tunnel reset to establish real capacity before this case.
    await workspace.restart()
    await installAvailabilityProbe(page, recordRead, () => { availabilityProbeFailures++ })
    const harvestReads = async () => {
      const journal = await page.evaluate(() => JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[])
      for (const event of journal)
        recordRead(event)
    }

    const events = (document: string, read: number) => reads.filter(event => event.document === document && event.read === read)
    const finishWithdrawal = async (label: string, response: ReturnType<typeof responseGate.hold>, document: string, read: number) => {
      expect(response.bodyReleased).toBe(false)
      if (!response.headersDelivered)
        response.releaseHeaders()
      await expect.poll(() => {
        responseGate.check()
        return response.closed && response.terminal === 'cancelled' && !response.bodyReleased
      }).toBe(true)
      withdrawalReceipts.push({
        label,
        document,
        read,
        headersDelivered: response.headersDelivered,
        bodyReleased: response.bodyReleased,
        terminal: response.terminal,
        closed: response.closed,
      })
      response.release()
    }

    const pendingReads = (document: string) => reads.filter(event => event.document === document && event.kind === 'start'
      && !events(document, event.read).some(outcome => ['body', 'rejected', 'body-rejected'].includes(outcome.kind)
        || (outcome.kind === 'headers' && outcome.status !== 200)))
    let expectedFailure: AvailabilityReadEvent | undefined
    let committedTerminal: AvailabilityReadEvent | undefined
    try {
      stage = 'initial direct reads'
      await page.clock.install()
      await page.goto(workspacePath('/mcps'))
      await authenticateWorkspace(page)
      await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
      const transport = page.locator('[data-transport-route]').first()
      await expect(transport).toHaveAttribute('data-transport-route', 'direct')
      const expectedAvailability = await page.evaluate(async () => {
        const reference = await fetch('/api/installations', { credentials: 'same-origin' })
        if (!reference.ok)
          throw new Error('The availability reference must be an authenticated success')
        window.cairnExpectedAvailability = await reference.json()
        return window.cairnExpectedAvailability
      })
      if (destination === 'committed') {
        await page.addInitScript((reference) => {
          window.cairnExpectedAvailability = reference
        }, expectedAvailability)
      }

      const departingDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      departingDocumentId = departingDocument
      await expect.poll(() => reads.some(event => event.document === departingDocument && event.kind === 'body')).toBe(true)
      await expect.poll(() => pendingReads(departingDocument)).toEqual([])
      await page.clock.pauseAt(new Date(Date.now() + 1000))
      await expect.poll(() => pendingReads(departingDocument)).toEqual([])
      const earlierReads = reads.filter(event => event.document === departingDocument && event.kind === 'start').length
      poll = destination === 'cancelled' ? responseGate.holdBody() : responseGate.hold('availability')
      const initialResponse = observeGate('initial-availability', poll)
      if (destination !== 'restored') {
        navigationResponse = responseGate.hold('agents')
        observeGate('first-navigation', navigationResponse)
      }

      stage = 'server holds the real availability response'
      await page.clock.runFor(3100)
      await expect.poll(() => {
        responseGate.check()
        return poll!.ready
      }).toBe(true)
      expect(poll.status).toBe(200)
      await expect.poll(() => reads.filter(event => event.document === departingDocument && event.kind === 'start').length).toBe(earlierReads + 1)
      const targetRead = reads.filter(event => event.document === departingDocument && event.kind === 'start').at(-1)!.read
      firstRead = targetRead
      initialResponse.read = targetRead
      if (destination === 'cancelled')
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'body-start').length).toBe(1)
      expect(events(departingDocument, targetRead).some(event => event.kind === 'abort')).toBe(false)
      await expect(transport).toHaveAttribute('data-transport-route', 'direct')
      // Restore ordinary production timers before a navigation can stall clock
      // evaluations. Subsequent polling/recovery must progress on its real cadence.
      await page.clock.resume()
      if (destination === 'restored') {
        // Exercise the actual same-document lifecycle handlers. Synthetic persisted
        // events do not claim that the engine physically cached this document.
        stage = 'same-document persisted pagehide'
        await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true })))
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'abort').map(event => event.afterPagehide)).toEqual([true])
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'rejected').map(event => event.aborted)).toEqual([true])
        expect(events(departingDocument, targetRead).filter(event => event.kind === 'body')).toEqual([])
        await finishWithdrawal('initial-availability', poll, departingDocument, targetRead)
        stage = 'same-document persisted pageshow resumes real polling'
        await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })))
        expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(departingDocument)
        await harvestReads()
        const restored = reads.filter(event => event.document === departingDocument && event.kind === 'pageshow' && event.persisted)
        expect(restored.map(event => event.trusted)).toEqual([false])
        await expect.poll(() => availabilityBodiesAfter(reads, departingDocument, restored[0]!.sequence).length).toBeGreaterThan(0)
        expect(reads.filter(event => event.document === departingDocument && event.persisted).map(event => event.kind)).toEqual(['pagehide', 'pageshow'])
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
      }
      else {
        stage = 'native destination response'
        navigation = page.goto(workspacePath('/agents'))
        const arrival = navigation.then(() => undefined, error => error)
        await expect.poll(() => {
          responseGate.check()
          return navigationResponse!.ready
        }).toBe(true)
        expect(navigationResponse!.status).toBe(200)
        expect(errors).toEqual([])
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'abort').map(event => ({
          trusted: event.trusted,
          exactTarget: event.exactTarget,
          insideAbortCall: event.insideAbortCall,
          afterPagehide: event.afterPagehide,
        }))).toEqual([{
          trusted: true,
          exactTarget: true,
          insideAbortCall: true,
          afterPagehide: false,
        }])
        const initialTerminal = destination === 'cancelled' ? 'body-rejected' : 'rejected'
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === initialTerminal).map(event => event.aborted)).toEqual([true])
        expect(events(departingDocument, targetRead).filter(event => event.kind === 'body')).toEqual([])
        await finishWithdrawal('initial-availability', poll, departingDocument, targetRead)
        if (destination === 'cancelled') {
          navigationResponse!.downloadNavigation()
          const failure = await arrival
          expect(failure).toBeInstanceOf(Error)
          expect(failure.message).toContain('Download is starting')
          await expect.poll(() => downloads.length).toBe(1)
          expect(downloads[0].suggestedFilename()).toBe('availability-navigation.html')
        }
        else {
          stage = 'delayed commit admits the next real periodic read'
          secondPoll = responseGate.hold('availability')
          const delayedResponse = observeGate('delayed-availability', secondPoll)
          await expect.poll(() => {
            responseGate.check()
            return secondPoll!.ready
          }).toBe(true)
          expect(secondPoll.status).toBe(200)
          await expect.poll(() => reads.filter(event => event.document === departingDocument && event.kind === 'start' && event.read > targetRead).length).toBe(1)
          secondRead = reads.filter(event => event.document === departingDocument && event.kind === 'start').at(-1)!.read
          delayedResponse.read = secondRead
          secondPoll.releaseHeaders()
          await expect.poll(() => events(departingDocument, secondRead!).filter(event => event.kind === 'body-start').length).toBe(1)
          navigationResponse!.release()
          expect(await arrival).toBeUndefined()
        }

        navigationValidated = true
        await harvestReads()
        // Bodies stay held until their original terminal and wire closure are checked.
      }

      if (destination === 'committed') {
        stage = 'native owned request withdrawal before pagehide'
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'abort').map(event => event.afterPagehide)).toEqual([false])
        await expect.poll(() => {
          responseGate.check()
          return poll!.closed && poll!.terminal === 'cancelled'
        }).toBe(true)
        expect(events(departingDocument, targetRead).filter(event => event.kind === 'body')).toEqual([])
        expect(secondRead).toBeDefined()
        stage = 'committed later read preserves its original terminal and document lifecycle'
        await expect.poll(() => events(departingDocument, secondRead!).filter(event => event.kind === 'body-rejected').length).toBe(1)
        const terminal = events(departingDocument, secondRead!).find(event => event.kind === 'body-rejected')!
        if (terminal.aborted) {
          expect(terminal.reasonMatches || terminal.errorName === 'AbortError').toBe(true)
          expect(events(departingDocument, secondRead!).filter(event => event.kind === 'abort').map(event => ({ trusted: event.trusted, exactTarget: event.exactTarget, insideAbortCall: event.insideAbortCall })))
            .toEqual([{ trusted: true, exactTarget: true, insideAbortCall: true }])
        }
        else {
          // This declared committed outcome retains the raw native failure.
          // It does not claim private abort ownership or app-generation evidence.
          expect(terminal).toMatchObject({
            kind: 'body-rejected',
            errorName: 'TypeError',
            aborted: false,
            reasonMatches: false,
          })
        }

        expect(events(departingDocument, secondRead!).filter(event => event.kind === 'body')).toEqual([])
        // This is an HTTP closure receipt, not evidence of a private JS abort.
        await finishWithdrawal('delayed-availability', secondPoll!, departingDocument, secondRead!)
        const departure = reads.filter(event => event.document === departingDocument && event.kind === 'pagehide')
        expect(departure).toHaveLength(1)
        expect(departure[0]).toMatchObject({ trusted: true, afterPagehide: true })
        expect(departure[0]!.sequence).toBeGreaterThan(events(departingDocument, secondRead!).find(event => event.kind === 'body-start')!.sequence)
        await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
        const freshDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
        expect(freshDocument).not.toBe(departingDocument)
        await expect.poll(() => availabilityBodiesAfter(reads, freshDocument, 0).length).toBeGreaterThan(0)
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
        // The complete journal remains in the receipt. Only this validated
        // committed-read terminal is accounted for separately in the audit.
        committedTerminal = terminal
      }
      else if (destination === 'cancelled') {
        stage = 'retained document resumes an exact replacement after proven native abort'
        await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
        expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(departingDocument)
        expect(events(departingDocument, targetRead).filter(event => event.kind === 'body')).toEqual([])
        await expect.poll(() => ({ closed: poll!.closed, terminal: poll!.terminal })).toEqual({ closed: true, terminal: 'cancelled' })
        await expect(transport).toHaveAttribute('data-transport-route', 'direct')
        stage = 'real periodic polling continues'
        const retained = await page.evaluate((selected) => {
          const journal = JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[]
          return {
            document: window.cairnAvailabilityDocument,
            sequence: Math.max(0, ...journal.filter(event => event.document === selected).map(event => event.sequence)),
          }
        }, departingDocument)
        expect(retained.document).toBe(departingDocument)
        expect(retained.sequence).toBeGreaterThan(0)
        retainedSequence = retained.sequence
        await expect.poll(() => availabilityBodiesAfter(reads, departingDocument, retainedSequence!).length).toBeGreaterThan(0)
        stage = 'genuine active HTTP failure'
        const unavailable = responseGate.unavailable()
        const failedResponse = observeGate('503-availability', unavailable)
        await expect.poll(() => unavailable.ready).toBe(true)
        expect(unavailable.status).toBe(200)
        await expect.poll(() => reads.filter(event => event.document === departingDocument && event.kind === 'headers' && event.status === 503).length).toBe(1)
        expectedFailure = reads.find(event => event.document === departingDocument && event.kind === 'headers' && event.status === 503)!
        failedResponse.read = expectedFailure.read
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Offline')
        await expect(transport).toHaveAttribute('data-transport-route', 'relay')
        stage = 'production backoff and direct recovery'
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
        await expect(transport).toHaveAttribute('data-transport-route', 'direct')
        stage = 'second real departure rearms the same document listener'
        await expect.poll(() => pendingReads(departingDocument)).toEqual([])
        const recoveredRead = reads.filter(event => event.document === departingDocument && event.kind === 'start').at(-1)!.read
        secondPoll = responseGate.hold('availability')
        const rearmedResponse = observeGate('rearmed-availability', secondPoll)
        await expect.poll(() => {
          responseGate.check()
          return secondPoll!.ready
        }).toBe(true)
        expect(secondPoll.status).toBe(200)
        await harvestReads()
        secondRead = reads.filter(event => event.document === departingDocument && event.kind === 'start').at(-1)!.read
        rearmedResponse.read = secondRead
        expect(secondRead).toBeGreaterThan(recoveredRead)
        expect(secondRead).toBeGreaterThan(targetRead)
        navigationResponse = responseGate.hold('agents')
        observeGate('second-navigation', navigationResponse)
        navigationValidated = false
        navigation = page.goto(workspacePath('/agents'))
        const secondArrival = navigation.then(() => undefined, error => error)
        await expect.poll(() => navigationResponse!.ready).toBe(true)
        await expect.poll(() => events(departingDocument, secondRead!).filter(event => event.kind === 'abort').map(event => ({
          trusted: event.trusted,
          exactTarget: event.exactTarget,
          insideAbortCall: event.insideAbortCall,
          afterPagehide: event.afterPagehide,
        }))).toEqual([{
          trusted: true,
          exactTarget: true,
          insideAbortCall: true,
          afterPagehide: false,
        }])
        await expect.poll(() => events(departingDocument, secondRead!).filter(event => event.kind === 'rejected').map(event => event.aborted)).toEqual([true])
        expect(events(departingDocument, secondRead!).filter(event => event.kind === 'body')).toEqual([])
        await finishWithdrawal('rearmed-availability', secondPoll, departingDocument, secondRead)
        navigationResponse.release()
        expect(await secondArrival).toBeUndefined()
        navigationValidated = true
        await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
        await expect.poll(() => secondPoll!.closed && secondPoll!.terminal === 'cancelled').toBe(true)
        await harvestReads()
      }

      stage = 'current direct readers and settled finite work'
      await expect(transport).toHaveAttribute('data-transport-route', 'direct')
      const currentDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      await expect.poll(() => pendingReads(currentDocument)).toEqual([])
      responseGate.check()
      stage = 'retire application and acknowledge its final journal'
      // The existing JSON health document has no application poll producers.
      // Keep same-origin storage accessible until departure receipts are read.
      const retirement = await page.goto(new URL('/health', workspace.url).href)
      expect(retirement?.status()).toBe(200)
      expect(retirement?.headers()['content-type']).toContain('application/json')
      const retiredDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      expect(retiredDocument).toBeTruthy()
      expect(retiredDocument).not.toBe(currentDocument)
      await expect(page.locator('[data-transport-route]')).toHaveCount(0)
      await harvestReads()
      expect(reads.filter(event => event.document === retiredDocument && event.kind === 'start')).toEqual([])
      journalAcknowledged = true
      stage = 'completed'
    }
    catch (error) {
      primaryFailure = error
    }
    finally {
      for (const gate of gates)
        gate.response.release()
      const closeOwned = async (operation: () => Promise<unknown>, actor: string) => {
        let deadline: ReturnType<typeof setTimeout> | undefined
        try {
          await Promise.race([
            Promise.resolve().then(operation),
            new Promise((_, reject) => deadline = setTimeout(() => reject(new Error(`Availability cleanup deadline: ${actor}`)), 7000)),
          ])
        }
        finally {
          clearTimeout(deadline)
        }
      }

      // Start independent owners before awaiting any of them: a blocked page
      // close must not prevent download cancellation or HTTP gate closure.
      const closures = [closeOwned(() => responseGate.close(), 'HTTP gate')]
      if (!page.isClosed())
        closures.push(closeOwned(() => page.close(), 'page'))
      for (const download of downloads) {
        closures.push((async () => {
          for (const operation of ['cancel', 'delete'] as const) {
            try {
              await closeOwned(() => download[operation](), `download ${operation}`)
            }
            catch (error) {
              cleanupFailures.push(error)
            }
          }
        })())
      }

      for (const closure of await Promise.allSettled(closures)) {
        if (closure.status === 'rejected')
          cleanupFailures.push(closure.reason)
      }

      // Page and HTTP closure can terminate a still-pending navigation. Never
      // gate those cleanup attempts on the navigation they need to terminate.
      try {
        await closeOwned(() => navigation || Promise.resolve(), 'navigation')
      }
      catch (error) {
        if (!navigationValidated)
          cleanupFailures.push(error)
      }
    }

    // Audit independent failure categories after every owned producer closes.
    // One late rejection must not discard an HTTP failure or a pageerror.
    const audits = [
      () => expect(incompleteAvailabilityReads(reads, availabilityProbeFailures)).toEqual([]),
      () => expect(reads.filter(event => ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted && event !== committedTerminal)).toEqual([]),
      () => expect(reads.filter(event => event.kind === 'headers' && event.status !== 200
        && !(expectedFailure && event.document === expectedFailure.document && event.read === expectedFailure.read && event.status === 503))).toEqual([]),
      () => expect(errors).toEqual([]),
    ]
    for (const audit of audits) {
      try {
        audit()
      }
      catch (error) {
        cleanupFailures.push(error)
      }
    }

    try {
      saveAvailabilityObservation(testInfo.outputPath('availability-native-observation.json'), JSON.stringify({
        stage,
        journalAcknowledged,
        departingDocument: departingDocumentId ?? null,
        firstRead: firstRead ?? null,
        secondRead: secondRead ?? null,
        retainedSequence: retainedSequence ?? null,
        expectedFailure: expectedFailure ?? null,
        committedTerminal: committedTerminal ?? null,
        navigationValidated,
        withdrawalReceipts,
        gates: gates.map(({
          label,
          document,
          read,
          response,
        }) => ({
          label,
          document,
          read,
          received: response.received,
          ready: response.ready,
          status: response.status ?? null,
          forcedStatus: label === '503-availability' ? 503 : null,
          terminal: response.terminal,
          closed: response.closed,
          headersDelivered: response.headersDelivered,
          bodyReleased: response.bodyReleased,
        })),
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
        secondPoll: secondPoll
          ? {
              received: secondPoll.received,
              ready: secondPoll.ready,
              status: secondPoll.status ?? null,
              terminal: secondPoll.terminal,
              closed: secondPoll.closed,
            }
          : null,
        navigation: navigationResponse
          ? {
              received: navigationResponse.received,
              ready: navigationResponse.ready,
              status: navigationResponse.status ?? null,
              terminal: navigationResponse.terminal,
              closed: navigationResponse.closed,
            }
          : null,
      }))
    }
    catch (error) {
      cleanupFailures.push(error)
    }

    if (primaryFailure || cleanupFailures.length)
      throw new AggregateError([...(primaryFailure ? [primaryFailure] : []), ...cleanupFailures], `Availability navigation failed at ${stage}`)
  })
}

availabilityTest('availability preserves a scoped readonly outage before native navigation', async ({ page, workspace, responseGate }, testInfo) => {
  const { createDirectOwnerObserver } = await import('../../test-results/direct-owner-receipt/direct-owner-probe.mjs')
  const directOwner = createDirectOwnerObserver(page, { installationId: workspace.installationId, installationSlug: workspace.installationSlug })
  let directOwnerPageClosed = false
  let directOwnerGateClosed = false
  const reads: AvailabilityReadEvent[] = []
  let availabilityProbeFailures = 0
  const errors: string[] = []
  const failures: unknown[] = []
  const record = (event: AvailabilityReadEvent) => {
    if (!reads.some(previous => previous.document === event.document && previous.sequence === event.sequence))
      reads.push(event)
  }

  const harvest = async () => {
    const snapshot = await page.evaluate(() => ({
      availability: JSON.parse(sessionStorage.getItem('cairn-e2e-availability-journal') || '[]') as AvailabilityReadEvent[],
      direct: (window as any).__c40DirectOwnerReceipt?.snapshot() ?? null,
    }))
    for (const event of snapshot.availability)
      record(event)
    directOwner.accept(snapshot.direct)
  }

  let stage = 'initial application'
  let original: string | undefined
  let target: number | undefined
  let retryRead: number | undefined
  let poll: ReturnType<typeof responseGate.hold> | undefined
  let outage: ReturnType<typeof responseGate.failAvailabilityReads> | undefined
  let expectedFailure: AvailabilityReadEvent | undefined
  const failureReceipts: AvailabilityReadEvent[] = []
  let faultWireReceipt: { closed: boolean, terminal: string, bodyReleased: boolean } | undefined
  let primaryFailure: unknown
  let journalAcknowledged = false
  const evidence = {
    attemptsBeforeFault: null as number | null,
    attemptsAfterTerminal: null as number | null,
    rejectedRequests: null as number | null,
    healthyRouteStatus: null as number | null,
    restoredBeforeNavigation: false,
  }
  page.on('pageerror', error => errors.push(error.message))
  try {
    await workspace.restart()
    await installAvailabilityProbe(page, record, () => { availabilityProbeFailures++ })
    await directOwner.install()
    await page.clock.install()
    await page.goto(workspacePath('/mcps'))
    await authenticateWorkspace(page)
    await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
    await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
    original = await page.evaluate(() => window.cairnAvailabilityDocument)
    await expect.poll(() => reads.some(event => event.document === original && event.kind === 'body')).toBe(true)
    await expect.poll(() => incompleteAvailabilityReads(reads, availabilityProbeFailures)).toEqual([])
    await page.clock.pauseAt(new Date(Date.now() + 1000))
    await harvest()
    const earlier = reads.filter(event => event.document === original && event.kind === 'start').length
    poll = responseGate.hold('availability')
    stage = 'real periodic read held before headers'
    await page.clock.runFor(3100)
    await expect.poll(() => {
      responseGate.check()
      return poll!.ready
    }).toBe(true)
    expect(poll.status).toBe(200)
    await harvest()
    const started = reads.filter(event => event.document === original && event.kind === 'start')
    expect(started.length).toBe(earlier + 1)
    target = started.at(-1)!.read
    const attemptsBeforeFault = responseGate.availabilityRequests
    evidence.attemptsBeforeFault = attemptsBeforeFault
    outage = responseGate.failAvailabilityReads()
    poll.disconnect()
    stage = 'original native failure during the owned readonly outage'
    await expect.poll(() => reads.filter(event => event.document === original && event.read === target && event.kind === 'rejected').length).toBe(1)
    expectedFailure = reads.find(event => event.document === original && event.read === target && event.kind === 'rejected')!
    expect(expectedFailure.errorName).toBe('TypeError')
    expect(expectedFailure.aborted).toBe(false)
    expect(expectedFailure.afterPagehide).toBe(false)
    failureReceipts.push(expectedFailure)
    expect(reads.filter(event => event.document === original && event.read === target && event.kind === 'abort')).toEqual([])
    await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Offline')
    stage = 'normal polling backoff remains inside the active readonly outage'
    // Advance only the application's existing six-second polling backoff.
    await page.clock.runFor(6100)
    await harvest()
    const retried = reads.filter(event => event.document === original && event.kind === 'start')
    expect(retried.length).toBe(earlier + 2)
    retryRead = retried.at(-1)!.read
    expect(retryRead).toBeGreaterThan(target)
    await expect.poll(() => reads.filter(event => event.document === original && event.read === retryRead && event.kind === 'rejected').length).toBe(1)
    const retryFailure = reads.find(event => event.document === original && event.read === retryRead && event.kind === 'rejected')!
    expect(retryFailure.errorName).toBe('TypeError')
    expect(retryFailure.aborted).toBe(false)
    expect(retryFailure.afterPagehide).toBe(false)
    expect(reads.filter(event => event.document === original && event.read === retryRead && event.kind === 'abort')).toEqual([])
    failureReceipts.push(retryFailure)
    await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Offline')
    evidence.healthyRouteStatus = await page.evaluate(async () => {
      const response = await fetch('/health', { credentials: 'same-origin' })
      await response.text()
      return response.status
    })
    expect(evidence.healthyRouteStatus).toBe(200)
    const rejectedRequests = outage.rejectedRequests
    expect(rejectedRequests).toBeGreaterThan(0)
    const attemptsAfterTerminal = responseGate.availabilityRequests
    evidence.rejectedRequests = rejectedRequests
    evidence.attemptsAfterTerminal = attemptsAfterTerminal
    expect(attemptsAfterTerminal).toBe(attemptsBeforeFault + rejectedRequests)
    await expect.poll(() => poll!.closed && poll!.terminal === 'cancelled' && !poll!.bodyReleased).toBe(true)
    faultWireReceipt = { closed: poll.closed, terminal: poll.terminal, bodyReleased: poll.bodyReleased }
    outage.restore()
    evidence.restoredBeforeNavigation = true
    stage = 'origin restored before the later native departure'
    await page.clock.resume()
    stage = 'restored origin serves the native destination'
    await page.goto(workspacePath('/agents'))
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
    await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
    expect(reads.filter(event => event.document === original && event.read === target && event.kind === 'abort')).toEqual([])
    await expect.poll(() => incompleteAvailabilityReads(reads, availabilityProbeFailures)).toEqual([])
    stage = 'retire application and acknowledge its final journal'
    await page.goto(new URL('/health', workspace.url).href)
    await harvest()
    journalAcknowledged = true
  }
  catch (cause) {
    primaryFailure = cause
  }
  finally {
    outage?.restore()
    poll?.release()
    const bounded = async (close: () => Promise<unknown>) => {
      let timer: ReturnType<typeof setTimeout> | undefined
      try {
        await Promise.race([
          Promise.resolve().then(close),
          new Promise((_, reject) => timer = setTimeout(() => reject(new Error('Outage owned cleanup deadline')), 7000)),
        ])
      }
      finally {
        clearTimeout(timer)
      }
    }

    const closed = await Promise.allSettled([bounded(async () => {
      await page.close()
      directOwnerPageClosed = page.isClosed()
      if (!directOwnerPageClosed)
        throw new Error('Direct owner page closure UNKNOWN')
    }), bounded(async () => {
      await responseGate.close()
      directOwnerGateClosed = true
    })])
    for (const result of closed) {
      if (result.status === 'rejected')
        failures.push(result.reason)
    }
  }

  const directOwnerCleanupFailureCount = failures.length
  const audits = [
    () => expect(incompleteAvailabilityReads(reads, availabilityProbeFailures)).toEqual([]),
    () => responseGate.check(),
    () => expect(reads.filter(event => ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted
      && !failureReceipts.some(expected => event.document === expected.document && event.sequence === expected.sequence))).toEqual([]),
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
    saveAvailabilityObservation(testInfo.outputPath('availability-outage-observation.json'), JSON.stringify({
      stage,
      original: original ?? null,
      target: target ?? null,
      retryRead: retryRead ?? null,
      expectedFailure: expectedFailure ?? null,
      failureReceipts,
      faultWireReceipt: faultWireReceipt ?? null,
      finalAvailabilityRequests: responseGate.availabilityRequests,
      finalRejectedRequests: outage?.rejectedRequests ?? null,
      poll: poll
        ? {
            received: poll.received,
            ready: poll.ready,
            status: poll.status ?? null,
            terminal: poll.terminal,
            closed: poll.closed,
            headersDelivered: poll.headersDelivered,
            bodyReleased: poll.bodyReleased,
          }
        : null,
      evidence,
      journalAcknowledged,
      reads,
      pageErrors: errors,
    }))
  }
  catch (cause) {
    failures.push(cause)
  }

  try {
    const observerRetired = directOwner.dispose()
    const receipt = directOwner.finish({ pageClosed: directOwnerPageClosed, gateClosed: directOwnerGateClosed, cleanupFailureCount: directOwnerCleanupFailureCount })
    saveAvailabilityObservation(testInfo.outputPath('direct-owner-private.json'), JSON.stringify(receipt.privateEvidence))
    saveAvailabilityObservation(testInfo.outputPath('direct-owner-projection.json'), JSON.stringify(receipt.projection))
    // A non-reproduced/unknown observation never becomes diagnostic success.
    if (receipt.projection.ownershipEvidenceScope !== 'PINNED_SOURCE_CONDITIONAL' || receipt.projection.universalRuntimeAdmission !== false || receipt.projection.runtimeProducerReceipt !== false || receipt.projection.proxyDetection !== 'NOT_PROVIDED' || receipt.projection.receiptStatus !== 'SOURCE_CONDITIONAL_PARTIAL' || !observerRetired || !receipt.projection.ownerReactionStateKnown || !receipt.projection.sourceCloseProvenanceKnown || !receipt.projection.firstErrorPairReproduced || !receipt.projection.closureKnown || !receipt.projection.pageClosed || !receipt.projection.gateClosed || receipt.projection.cleanupFailureCount !== 0)
      failures.push(new Error('Source-conditional Direct receipt remains UNKNOWN'))
  }
  catch (cause) {
    failures.push(cause)
  }

  if (primaryFailure || failures.length)
    throw new AggregateError([...(primaryFailure ? [primaryFailure] : []), ...failures], 'Application availability outage qualification failed')
})
