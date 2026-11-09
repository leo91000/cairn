import { writeFileSync as saveAvailabilityObservation } from 'node:fs'
import { mcpProvider } from '../fixtures/mcp-provider'
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

interface AvailabilityReadEvent {
  kind: 'start' | 'headers' | 'body' | 'rejected' | 'body-rejected' | 'abort' | 'pagehide' | 'pageshow'
  document: string
  read: number
  status?: number
  aborted: boolean
  afterPagehide: boolean
  persisted?: boolean
}

type OAuthDepartureProbe = { kind: 'background' } | { kind: 'offer', closed: boolean }

declare global {
  interface Window {
    cairnOAuthProbe: (probe: OAuthDepartureProbe) => Promise<void>
    cairnAvailabilityDocument: string
    cairnAvailabilityRecord: (event: AvailabilityReadEvent) => Promise<void>
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
    const closedOffers: boolean[] = []
    const offerCompleted = Promise.withResolvers<void>()
    await page.exposeFunction('cairnOAuthProbe', (probe: OAuthDepartureProbe) => {
      if (probe.kind === 'background') {
        backgroundRequests++
        return
      }

      closedOffers.push(probe.closed)
      offerCompleted.resolve()
    })
    await page.clock.install()
    await page.addInitScript((holdOffer) => {
      let departing = false
      const request = window.fetch
      window.fetch = (...args) => {
        const path = args[0]
        if (typeof path === 'string' && path.endsWith('/direct/authorize')) {
          const signal = args[1]?.signal
          if (signal?.aborted) {
            departing = true
          }
          else {
            signal?.addEventListener('abort', () => {
              departing = true
            }, { once: true })
          }
        }

        const background = typeof path === 'string' && (path === '/api/installations' || path.endsWith('/tasks/activity') || path.endsWith('/direct/authorize'))
        if (background && departing)
          void window.cairnOAuthProbe({ kind: 'background' })
        return request(...args)
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
          void window.cairnOAuthProbe({ kind: 'offer', closed: peerClosed() })
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
    let stage = 'fixture reset'
    let primaryFailure: unknown
    const cleanupFailures: unknown[] = []
    let poll: ReturnType<typeof responseGate.hold> | undefined
    let navigationResponse: ReturnType<typeof responseGate.hold> | undefined
    let navigation: Promise<unknown> | undefined
    page.on('pageerror', error => errors.push(error.message))
    // Accepted grants belong to this worker's earlier pages until expiry. Use
    // the existing tunnel reset to establish real capacity before this case.
    await workspace.restart()
    await page.exposeFunction('cairnAvailabilityRecord', (event: AvailabilityReadEvent) => reads.push(event))
    await page.addInitScript(() => {
      const documentId = crypto.randomUUID()
      window.cairnAvailabilityDocument = documentId
      let pageHidden = false
      let sequence = 0
      for (const kind of ['pagehide', 'pageshow'] as const) {
        window.addEventListener(kind, (event) => {
          pageHidden = kind === 'pagehide'
          void window.cairnAvailabilityRecord({
            kind,
            document: documentId,
            read: 0,
            aborted: false,
            afterPagehide: pageHidden,
            persisted: event.persisted,
          })
        })
      }

      const request = window.fetch
      window.fetch = (...args) => {
        if (args[0] !== '/api/installations')
          return request(...args)
        const read = ++sequence
        const signal = args[1]?.signal
        const record = (kind: AvailabilityReadEvent['kind'], status?: number) => {
          void window.cairnAvailabilityRecord({
            kind,
            document: documentId,
            read,
            status,
            aborted: !!signal?.aborted,
            afterPagehide: pageHidden,
          })
        }

        // Report completion after the application's await/catch/finally cycle,
        // without replacing or waiting on its native fetch/body promises.
        const settled = (kind: AvailabilityReadEvent['kind']) => queueMicrotask(() => record(kind))
        signal?.addEventListener('abort', () => record('abort'), { once: true })
        record('start')
        const pending = request(...args)
        void pending.then((response) => {
          record('headers', response.status)
          const json = response.json.bind(response)
          response.json = () => {
            const body = json()
            void body.then(() => settled('body'), () => settled('body-rejected'))
            return body
          }
        }, () => settled('rejected'))
        return pending
      }
    })
    const events = (document: string, read: number) => reads.filter(event => event.document === document && event.read === read)
    const pendingReads = (document: string) => reads.filter(event => event.document === document && event.kind === 'start'
      && !events(document, event.read).some(outcome => ['body', 'rejected', 'body-rejected', 'abort'].includes(outcome.kind)
        || (outcome.kind === 'headers' && outcome.status !== 200)))
    let expectedFailure: AvailabilityReadEvent | undefined
    try {
      stage = 'initial direct reads'
      await page.clock.install()
      await page.goto(workspacePath('/mcps'))
      await authenticateWorkspace(page)
      await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
      const transport = page.locator('[data-transport-route]').first()
      await expect(transport).toHaveAttribute('data-transport-route', 'direct')
      const departingDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      await expect.poll(() => reads.some(event => event.document === departingDocument && event.kind === 'body')).toBe(true)
      await expect.poll(() => pendingReads(departingDocument)).toEqual([])
      await page.clock.pauseAt(new Date(Date.now() + 1000))
      await expect.poll(() => pendingReads(departingDocument)).toEqual([])
      const earlierReads = reads.filter(event => event.document === departingDocument && event.kind === 'start').length
      poll = responseGate.hold('availability')
      if (destination !== 'restored')
        navigationResponse = responseGate.hold('agents')
      stage = 'server holds the real availability response'
      await page.clock.runFor(3100)
      await expect.poll(() => {
        responseGate.check()
        return poll!.ready
      }).toBe(true)
      expect(poll.status).toBe(200)
      await expect.poll(() => reads.filter(event => event.document === departingDocument && event.kind === 'start').length).toBe(earlierReads + 1)
      const targetRead = reads.filter(event => event.document === departingDocument && event.kind === 'start').at(-1)!.read
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
        await expect.poll(() => poll!.terminal).toBe('cancelled')
        expect(events(departingDocument, targetRead).filter(event => event.kind === 'body')).toEqual([])
        poll.release()
        stage = 'same-document persisted pageshow resumes real polling'
        await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })))
        expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(departingDocument)
        await expect.poll(() => reads.filter(event => event.document === departingDocument && event.read > targetRead && event.kind === 'body').length).toBeGreaterThan(0)
        expect(reads.filter(event => event.document === departingDocument && event.persisted).map(event => event.kind)).toEqual(['pagehide', 'pageshow'])
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
      }
      else {
        stage = 'native destination response'
        navigation = page.goto(workspacePath('/agents'))
        const arrival = navigation.then(() => 'committed', () => 'cancelled')
        await expect.poll(() => {
          responseGate.check()
          return navigationResponse!.ready
        }).toBe(true)
        expect(navigationResponse!.status).toBe(200)
        expect(errors).toEqual([])
        if (destination === 'cancelled')
          navigationResponse!.cancelNavigation()
        else
          navigationResponse!.release()
        expect(await arrival).toBe(destination)
        // Always release owned HTTP bytes before waiting for their terminal receipt.
        poll.release()
      }

      if (destination === 'committed') {
        stage = 'caller and native request withdrawal after pagehide'
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'abort').map(event => event.afterPagehide)).toEqual([true])
        await expect.poll(() => poll!.terminal).toBe('cancelled')
        expect(events(departingDocument, targetRead).filter(event => event.kind === 'body')).toEqual([])
        await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
        expect(await page.evaluate(() => window.cairnAvailabilityDocument)).not.toBe(departingDocument)
      }
      else if (destination === 'cancelled') {
        stage = 'retained document consumes its actual fetch body'
        await expect(page.getByRole('heading', { name: 'MCPs', exact: true })).toBeVisible()
        expect(await page.evaluate(() => window.cairnAvailabilityDocument)).toBe(departingDocument)
        await expect.poll(() => events(departingDocument, targetRead).filter(event => event.kind === 'body').length).toBe(1)
        expect(events(departingDocument, targetRead).some(event => event.kind === 'abort' || event.kind === 'rejected' || event.kind === 'body-rejected')).toBe(false)
        await expect.poll(() => poll!.terminal).toBe('finished')
        await expect(transport).toHaveAttribute('data-transport-route', 'direct')
        stage = 'real periodic polling continues'
        await expect.poll(() => reads.filter(event => event.document === departingDocument && event.read > targetRead && event.kind === 'body').length).toBeGreaterThan(0)
        stage = 'genuine active HTTP failure'
        const unavailable = responseGate.unavailable()
        await expect.poll(() => unavailable.ready).toBe(true)
        expect(unavailable.status).toBe(200)
        await expect.poll(() => reads.filter(event => event.document === departingDocument && event.kind === 'headers' && event.status === 503).length).toBe(1)
        expectedFailure = reads.find(event => event.document === departingDocument && event.kind === 'headers' && event.status === 503)!
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Offline')
        await expect(transport).toHaveAttribute('data-transport-route', 'relay')
        stage = 'production backoff and direct recovery'
        await expect(page.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
      }

      stage = 'current direct readers and settled finite work'
      await expect(transport).toHaveAttribute('data-transport-route', 'direct')
      const currentDocument = await page.evaluate(() => window.cairnAvailabilityDocument)
      await expect.poll(() => pendingReads(currentDocument)).toEqual([])
      responseGate.check()
      stage = 'completed'
    }
    catch (error) {
      primaryFailure = error
    }
    finally {
      poll?.release()
      navigationResponse?.release()
      try {
        await navigation?.catch(() => {})
        if (!page.isClosed())
          await page.close()
      }
      catch (error) {
        cleanupFailures.push(error)
      }

      try {
        await responseGate.close()
      }
      catch (error) {
        cleanupFailures.push(error)
      }
    }

    // Every owned producer is closed before evaluating the complete journal.
    // Keep the original failure first while retaining late HTTP/native failures.
    try {
      expect(reads.filter(event => ['rejected', 'body-rejected'].includes(event.kind) && !event.aborted)).toEqual([])
      expect(reads.filter(event => event.kind === 'headers' && event.status !== 200
        && !(expectedFailure && event.document === expectedFailure.document && event.read === expectedFailure.read && event.status === 503))).toEqual([])
      expect(errors).toEqual([])
    }
    catch (error) {
      cleanupFailures.push(error)
    }

    try {
      saveAvailabilityObservation(testInfo.outputPath('availability-native-observation.json'), JSON.stringify({
        stage,
        reads,
        pageErrors: errors.length,
        poll: poll && { ready: poll.ready, status: poll.status, terminal: poll.terminal },
        navigation: navigationResponse && { ready: navigationResponse.ready, status: navigationResponse.status, terminal: navigationResponse.terminal },
      }))
    }
    catch (error) {
      cleanupFailures.push(error)
    }

    if (primaryFailure || cleanupFailures.length)
      throw new AggregateError([...(primaryFailure ? [primaryFailure] : []), ...cleanupFailures], `Availability navigation failed at ${stage}`)
  })
}
