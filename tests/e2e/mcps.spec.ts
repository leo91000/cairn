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

type OAuthDepartureProbe = { kind: 'background' } | { kind: 'offer', closed: boolean }

interface CallbackBootstrapProbe {
  kind: 'session-held' | 'load' | 'retired' | 'pageshow' | 'authorize'
  documentId: string
  retired: boolean
  trusted?: boolean
  persisted?: boolean
  sessionHeld?: boolean
}

declare global {
  interface Window {
    cairnOAuthProbe: (probe: OAuthDepartureProbe) => Promise<void>
    cairnCallbackBootstrapProbe: (probe: CallbackBootstrapProbe) => Promise<void>
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

test('initial pageshow cannot restart direct authorization during OAuth callback canonicalization', async ({ page, workspace, browserName }) => {
  test.skip(browserName !== 'webkit', 'Exercises the observed native WebKit load/pageshow ordering')
  const provider = await mcpProvider()
  const probes: CallbackBootstrapProbe[] = []
  const errors: string[] = []
  const sessionHeld = Promise.withResolvers<void>()
  const callbackPath = `/installations/${workspace.installationId}/mcps/callback`
  page.on('pageerror', error => errors.push(error.message))
  await page.exposeFunction('cairnCallbackBootstrapProbe', (probe: CallbackBootstrapProbe) => {
    probes.push(probe)
    if (probe.kind === 'session-held')
      sessionHeld.resolve()
  })
  await page.addInitScript((legacyCallbackPath) => {
    if (location.pathname !== legacyCallbackPath)
      return
    const documentId = crypto.randomUUID()
    let retired = false
    let releaseSession: (() => void) | undefined
    const report = (probe: Omit<CallbackBootstrapProbe, 'documentId' | 'retired'>) => {
      void window.cairnCallbackBootstrapProbe({ documentId, retired, ...probe })
    }

    // Observe the real departing peer; keep its native offer and close behavior.
    const close = RTCPeerConnection.prototype.close
    RTCPeerConnection.prototype.close = function () {
      Reflect.apply(close, this, [])
      retired = true
      report({ kind: 'retired' })
    }

    // Deliver the real session response at the native load checkpoint so the
    // production account shell canonicalizes before initial native pageshow.
    window.addEventListener('load', (event) => {
      report({ kind: 'load', trusted: event.isTrusted, sessionHeld: !!releaseSession })
      releaseSession?.()
    }, { once: true })
    window.addEventListener('pageshow', event => report({ kind: 'pageshow', trusted: event.isTrusted, persisted: event.persisted }))
    const fetch = window.fetch
    window.fetch = (...args) => {
      const resource = args[0]
      const address = typeof resource === 'string' || resource instanceof URL ? resource : resource.url
      const pathname = new URL(address, location.href).pathname
      if (pathname.endsWith('/direct/authorize'))
        report({ kind: 'authorize' })

      const pending = fetch(...args)
      if (pathname !== '/api/account/session')
        return pending

      return pending.then(async (response) => {
        const text = await response.text()
        return new Promise<Response>((resolve) => {
          releaseSession = () => resolve(new Response(text, { status: response.status, headers: response.headers }))
          report({ kind: 'session-held' })
        })
      })
    }
  }, callbackPath)
  await page.route(url => url.pathname === callbackPath, async (route) => {
    const response = await route.fetch()
    const body = (await response.text()).replace('</body>', '<img alt="" src="/__cairn-callback-load.svg"></body>')
    await route.fulfill({ response, body })
  })
  await page.route('**/__cairn-callback-load.svg', async (route) => {
    await sessionHeld.promise
    await route.fulfill({ contentType: 'image/svg+xml', body: '<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>' })
  })
  try {
    const name = 'Callback lifecycle'
    await workspace.api('/api/mcps', 'POST', {
      name,
      transport: 'http',
      url: `${provider.origin}/mcp`,
      auth: 'oauth',
      allowPrivateNetwork: true,
    })
    await page.goto(workspacePath('/mcps'))
    await authenticateWorkspace(page)
    const card = page.locator('.mcp-card').filter({ hasText: name })
    await card.getByRole('button', { name: 'Connect', exact: true }).click()
    await expect(card.getByText('Connected', { exact: true })).toBeVisible()
    expect(provider.exchanges).toBe(1)
    expect(probes.filter(probe => probe.kind === 'load')).toEqual([
      expect.objectContaining({ trusted: true, sessionHeld: true }),
    ])
    expect(probes.filter(probe => probe.kind === 'pageshow')).toEqual([
      expect.objectContaining({ trusted: true, persisted: false, retired: true }),
    ])
    expect(probes.filter(probe => probe.kind === 'authorize' && probe.retired)).toEqual([])
    expect(errors).toEqual([])
  }
  finally {
    sessionHeld.resolve()
    try {
      await page.unrouteAll({ behavior: 'wait' })
    }
    finally {
      await provider.close()
    }
  }
})

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
