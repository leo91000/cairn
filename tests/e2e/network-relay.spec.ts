import type { Browser, Page } from '@playwright/test'
import { execFileSync, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import process from 'node:process'
import { chromium, expect, test } from '@playwright/test'
import { closeNetworkFixture, networkPhaseDiagnostics } from '../../scripts/network-phase-diagnostics.mjs'
import { beaconRelayFixture } from './beacon-relay-fixture'
import { captureNetworkTransport, networkTransportDiagnostics } from './network-transport-diagnostics'

test('authenticated browser and Rust client keep using the observed route under network constraints', async () => {
  test.setTimeout(240000)
  const phases = networkPhaseDiagnostics(process.env.CAIRN_NETWORK_OUTPUT)
  phases.enter('fixture-start')
  const fixture = await beaconRelayFixture(4398, '198.18.103.1', process.env.CAIRN_NETWORK_FIXTURE_DIRECTORY)
  const {
    root,
    url,
    messages,
    start,
    beacon,
  } = fixture
  let browser: Browser | undefined
  const evidence: {
    route: string
    operation: string
    elapsedMs: number
    directFailure?: { phase: string, timedOut: boolean } | null
    fallbackReason?: 'heartbeat-timeout' | 'opening-timeout' | 'peer-failed'
    directPhases?: { event: string, elapsedMs: number, remainingMs: number }[]
    failureHttpStatus?: number | null
  }[] = []
  const revocations: string[] = []
  const oldDeliveryPrerequisites: Record<string, boolean> = {}

  async function rustRequest(path: string, cookie: string, status: number, marker?: string) {
    const directRead = status === 200 && marker !== undefined
    const executable = directRead ? process.env.CAIRN_NETWORK_DIRECT_CLIENT! : process.env.CAIRN_NETWORK_RUST_CLIENT!
    const args = directRead
      ? ['http://localhost:4398', path.split('/')[3]!, path.slice(path.indexOf('/api/', 5)), marker]
      : ['http', '127.0.0.1:4398', path, String(status), ...(marker ? [marker] : [])]
    const child = spawn(executable, args, { stdio: ['pipe', 'pipe', 'pipe'] })
    let output = ''
    let diagnostic = ''
    child.stdout.on('data', chunk => output += chunk)
    child.stderr.on('data', chunk => diagnostic += chunk)
    const exited = once(child, 'exit')
    child.stdin.end(cookie)
    const [code] = await exited
    expect(code, diagnostic).toBe(0)
    return JSON.parse(output)
  }

  try {
    phases.enter('beacon-start')
    let service = beacon()
    // Exercise numeric ICE paths deterministically; the separate mDNS-only client
    // case preserves Chromium's default privacy behavior and expects the relay.
    phases.enter('browser-launch')
    browser = await chromium.launch({
      executablePath: process.env.CAIRN_NETWORK_CHROMIUM,
      args: process.env.CAIRN_NETWORK_SCENARIO === 'mdns-only-client' ? [] : ['--disable-features=WebRtcHideLocalIpsWithMdns'],
    })
    browser.on('disconnected', () => phases.event('browser-disconnected'))
    phases.enter('browser-page')
    const page = await browser.newPage()
    page.context().on('close', () => phases.event('context-closed'))
    page.on('close', () => phases.event('page-closed'))
    page.on('crash', () => phases.event('page-crashed'))
    const leases: string[] = []
    page.on('response', async (response) => {
      if (response.url().endsWith('/direct/authorize') && response.ok()) {
        const authorization = await response.json().catch(() => undefined)
        if (authorization?.available)
          leases.push(authorization.grant.claims.connection_id)
      }
    })

    interface Observation {
      route: string
      path: string
      method: string
      cursor?: number
      observedAt: number
    }

    async function capture(targetPage: Page) {
      await captureNetworkTransport(targetPage)
      await targetPage.addInitScript(() => {
        const target = window as typeof window & { transportObservations: unknown[] }
        target.transportObservations = []
        window.addEventListener('cairn-transport-observation', (event) => {
          target.transportObservations.push({ ...(event as CustomEvent).detail, observedAt: performance.now() })
          if (target.transportObservations.length > 200)
            target.transportObservations.shift()
        })
      })
    }

    phases.enter('capture-transport')
    await capture(page)
    if (process.env.CAIRN_NETWORK_SCENARIO === 'same-server') {
      // Slow outbound trickle signaling while the browser's UDP checks keep
      // running. An eager public alias is therefore checked before the
      // installation learns a remote destination, just like production #163.
      await page.route('**/direct/*/signal', async (route) => {
        if (route.request().postDataJSON()?.kind === 'candidate')
          await new Promise(resolve => setTimeout(resolve, 750))
        await route.continue()
      })
    }

    if (process.env.CAIRN_NETWORK_SCENARIO === 'mdns-only-client') {
      await page.addInitScript(() => {
        const browser = window as typeof window & { iceCandidates: { mdns: boolean, type: string | null }[] }
        browser.iceCandidates = []
        browser.RTCPeerConnection = new Proxy(browser.RTCPeerConnection, {
          construct(target, args) {
            const peer = Reflect.construct(target, args) as RTCPeerConnection
            peer.addEventListener('icecandidate', (event) => {
              if (event.candidate && browser.iceCandidates.length < 128) {
                browser.iceCandidates.push({
                  mdns: event.candidate.candidate.split(' ')[4]?.endsWith('.local') || false,
                  type: event.candidate.type,
                })
              }
            })
            return peer
          },
        })
      })
    }

    async function activeStream(target: Page, route: string) {
      await expect.poll(() => target.evaluate(route => (window as typeof window & { transportObservations: Observation[] }).transportObservations.some(item => item.method === 'STREAM' && item.route === route), route)).toBe(true)
    }

    const observations = () => page.evaluate(() => (window as typeof window & { transportObservations: Observation[] }).transportObservations)
    const expected = process.env.CAIRN_NETWORK_EXPECT_ROUTE || 'relay'
    let releaseDirect: () => void = () => {}
    const authorizationGate = new Promise<void>(resolve => releaseDirect = resolve)
    phases.enter('route-setup')
    await page.route('**/direct/authorize', async (route) => {
      await authorizationGate
      await route.continue()
    })
    phases.enter('beacon-health')
    await expect.poll(() => {
      expect(service.exitCode, 'beacon binary must remain running').toBeNull()
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }).toBe(true)
    phases.enter('beacon-stun')
    await expect.poll(() => fetch(`${url}/health`).then(response => response.json()).then(health => health.stun.status)).toBe('running')
    phases.enter('page-navigate')
    await page.goto(url)
    const ownerEmail = `network-${process.env.CAIRN_NETWORK_SCENARIO}-${Date.now()}@example.test`
    phases.enter('login-email')
    await page.getByLabel('Email address').fill(ownerEmail)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    phases.enter('login-code')
    await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
    phases.enter('login-submit')
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    phases.enter('claim-open')
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    phases.enter('installation-start')
    const installation = start('target/debug/cairn', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      HOST: '127.0.0.1',
      PORT: '4399',
      CAIRN_BEACON_ORIGIN: url,
      CAIRN_INSTALLATION_CLAIM_CODE: code,
      CAIRN_INSTALLATION_NAME: 'Network bench installation',
    })
    const bootstrapStart = performance.now()
    phases.enter('installation-bootstrap')
    await expect(async () => {
      expect(installation.exitCode).toBeNull()
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass({ timeout: 30000 })
    phases.enter('route-bootstrap')
    await expect(page.getByRole('link', { name: 'New conversation', exact: true }).first()).toBeVisible()
    await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'relay')
    await expect.poll(async () => (await observations()).some(item => item.route === 'relay' && item.method === 'GET')).toBe(true)
    evidence.push({ route: (await observations()).find(item => item.method === 'GET')!.route, operation: 'bootstrap-read', elapsedMs: performance.now() - bootstrapStart })
    const packetLoss = process.env.CAIRN_NETWORK_SCENARIO === 'packet-loss'
    if (!packetLoss)
      releaseDirect()
    const installationKey = new URL(page.url()).pathname.split('/')[2]!
    phases.enter('account-session')
    const account = await (await page.request.get(`${url}/api/account/session`)).json()
    const installationId = account.installations.find((item: { id: string, slug: string }) => item.id === installationKey || item.slug === installationKey)!.id
    const chatsPath = `/api/installations/${installationId}/api/chats`
    const marker = `Network scenario ${process.env.CAIRN_NETWORK_SCENARIO}`
    phases.enter('conversation-open')
    if (packetLoss) {
      // Chat creation is not safely replayable after transport loss. Prepare it
      // on the authenticated relay, then exercise the idempotent message below.
      const setupSession = await (await page.request.get(`${url}/api/account/session`)).json()
      const creationStarted = performance.now()
      const created = await page.request.post(`${url}${chatsPath}`, {
        headers: { 'origin': url, 'x-csrf-token': setupSession.csrf },
        data: {},
      })
      expect(created.ok()).toBe(true)
      const creationRoute = created.headers()['x-cairn-transport']!
      expect(creationRoute).toBe('relay')
      const chat = await created.json()
      evidence.push({ route: creationRoute, operation: 'bootstrap-create', elapsedMs: performance.now() - creationStarted })
      await page.goto(`${url}/installations/${installationId}/chats/${chat.id}`)
      releaseDirect()
    }
    else {
      await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    }

    phases.enter('message-fill')
    await page.getByLabel('Message', { exact: true }).fill(marker)
    phases.enter('route-promote')
    if (packetLoss) {
      // This global schedule supplies no per-fragment delivery bound. Observe
      // promotion or a diagnosed failed opening inside the original window.
      await expect.poll(async () => {
        const route = await page.getByRole('status', { name: 'Connection route' }).getAttribute('data-transport-route')
        const diagnostics = await networkTransportDiagnostics(page)
        return route === 'direct' || diagnostics.some(item => item.kind === 'channel-close')
      }, { timeout: 35000 }).toBe(true)
      oldDeliveryPrerequisites.promotion = await page.getByRole('status', { name: 'Connection route' }).getAttribute('data-transport-route') === 'direct'
    }
    else {
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', expected, { timeout: 35000 })
    }

    const before = (await observations()).length
    const responseObservationOptions = packetLoss ? { timeout: 35000 } : {}
    const started = performance.now()
    phases.enter('message-send')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    phases.enter('message-delivery')
    await expect.poll(async () => (await observations()).slice(before).some(item => item.method === 'POST' && item.path.endsWith('/messages')), responseObservationOptions).toBe(true)
    const sent = (await observations()).slice(before).find(item => item.method === 'POST' && item.path.endsWith('/messages'))!
    let fallbackReason: 'heartbeat-timeout' | 'opening-timeout' | 'peer-failed' | undefined
    if (packetLoss) {
      const diagnostics = await networkTransportDiagnostics(page) as {
        kind: string
        elapsedMs: number
        peer?: number
        ice?: string
        connection?: string
      }[]
      const prior = diagnostics.filter(item => item.elapsedMs <= sent.observedAt)
      const heartbeat = prior.findLast(item => item.kind === 'heartbeat-timeout')
      oldDeliveryPrerequisites.completedDirectBeforeFallback = (await observations()).some(item => item.route === 'direct' && ['GET', 'POST', 'STREAM'].includes(item.method) && item.observedAt < (heartbeat?.elapsedMs ?? sent.observedAt))
      if (sent.route === 'relay') {
        const openingClose = prior.findLast(item => item.kind === 'channel-close'
          && !prior.some(event => event.peer === item.peer && event.kind === 'channel-open'))
        const created = prior.find(item => item.kind === 'created' && item.peer === openingClose?.peer)
        const openingTimeout = openingClose && created && openingClose.elapsedMs - created.elapsedMs >= 29500
        const failed = prior.some(item => item.ice === 'failed' || item.connection === 'failed')
        fallbackReason = heartbeat ? 'heartbeat-timeout' : openingTimeout ? 'opening-timeout' : failed ? 'peer-failed' : undefined
        expect(fallbackReason, 'stress fallback requires an observed health/opening failure').toBeDefined()
      }

      oldDeliveryPrerequisites.browserFallback = sent.route === 'direct'
        || (fallbackReason === 'heartbeat-timeout' && oldDeliveryPrerequisites.completedDirectBeforeFallback)
    }

    // The title is delivered asynchronously on the same reliable transport;
    // packet loss can delay it beyond 10 s without exceeding reassembly's 30 s.
    phases.enter('message-heading')
    await expect(page.getByRole('heading', { name: marker, exact: true })).toBeVisible(responseObservationOptions)
    evidence.push({
      route: sent.route,
      operation: 'browser-send',
      elapsedMs: performance.now() - started,
      fallbackReason,
    })
    if (packetLoss) {
      // Require a real accepted batch and one stored submission; response headers
      // and a route indicator cannot prove recovered application delivery.
      const chatId = new URL(page.url()).pathname.split('/').at(-1)!
      // The committed message batch can precede its independent POST response.
      // The rendered title above proves delivery of this submission's state;
      // require accepted stream traffic since submission began, in either order.
      const acceptedSubmissionBatch = async () => (await observations()).slice(before).find(item => item.method === 'STREAM' && item.path === `/chats/${chatId}/stream`)
      await expect.poll(acceptedSubmissionBatch, responseObservationOptions).toBeTruthy()
      const detail = await (await page.request.get(`${url}${chatsPath}/${chatId}`)).json()
      expect(detail.messages.filter((message: { text: string }) => message.text === marker)).toHaveLength(1)
    }

    const cookies = await page.context().cookies()
    const cookie = cookies.map(value => `${value.name}=${value.value}`).join('; ')
    const samples = process.env.CAIRN_NETWORK_SCENARIO === 'packet-loss' ? 3 : 1
    phases.enter('rust-read')
    for (let sample = 0; sample < samples; sample++)
      evidence.push({ ...await rustRequest(chatsPath, cookie, 200, marker), operation: sample ? `rust-read-${sample + 1}` : 'rust-read' })
    phases.enter('anonymous-rust-read')
    await rustRequest(chatsPath, '', 401)
    // A loopback listener on the installation still refuses anonymous local access.
    phases.enter('anonymous-local-read')
    const local = execFileSync(process.env.CAIRN_NETWORK_LOCAL_CLIENT!, ['http', '127.0.0.1:4399', '/api/chats', '401'], { input: '', encoding: 'utf8' })
    expect(JSON.parse(local).status).toBe(401)
    phases.enter('owner-session')
    const session = await (await page.request.get(`${url}/api/account/session`)).json()
    phases.enter('scenario-verification')

    if (process.env.CAIRN_NETWORK_SCENARIO === 'network-change') {
      const beforeChange = (await observations()).length
      const changed = performance.now()
      execFileSync(process.env.CAIRN_NETWORK_CHANGE!, [], { stdio: 'ignore' })
      // Publish through the public relay from a separate authenticated control
      // client. Fresh content in the original page demonstrates stream recovery;
      // retained DOM and successful HTTP headers alone are insufficient.
      const fresh = 'Delivered after the client changed network'
      const chatId = new URL(page.url()).pathname.split('/').at(-1)!
      const published = await page.request.post(`${url}/api/installations/${installationId}/api/chats/${chatId}/messages`, {
        headers: { 'origin': url, 'x-csrf-token': session.csrf },
        data: { id: randomUUID(), text: fresh },
      })
      expect(published.status()).toBe(200)

      await expect(page.getByText(fresh, { exact: true })).toBeVisible({ timeout: 30000 })
      const resumed = (await observations()).slice(beforeChange).filter(item => item.method === 'STREAM' && item.path === `/chats/${chatId}/stream`).at(-1)!
      expect(resumed).toBeTruthy()
      evidence.push({ route: resumed.route, operation: 'stream-resume', elapsedMs: performance.now() - changed })
      expect(evidence.at(-1)!.elapsedMs).toBeLessThan(30000)
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', expected, { timeout: 35000 })
      evidence.push({ ...await rustRequest(chatsPath, cookie, 200, marker), operation: 'rust-after-change' })
      await expect(page.getByRole('heading', { name: marker, exact: true })).toBeVisible()
    }

    if (process.env.CAIRN_NETWORK_SCENARIO === 'same-lan') {
      // Browser-facing denial seam: Chromium's LNA permission is denied too,
      // but this loopback fixture is not a public-to-local origin. Inject only
      // the WebRTC NotAllowedError, leaving all application/relay traffic real.
      const denied = await browser.newContext({ storageState: await page.context().storageState() })
      const deniedPage = await denied.newPage()
      const errors: string[] = []
      deniedPage.on('pageerror', error => errors.push(error.name))
      await capture(deniedPage)
      await deniedPage.addInitScript(() => {
        RTCPeerConnection.prototype.createOffer = () => Promise.reject<never>(new DOMException('Local network access denied', 'NotAllowedError'))
      })
      const permission = await denied.newCDPSession(deniedPage)
      await permission.send('Browser.setPermission', {
        permission: { name: 'local-network-access' },
        setting: 'denied',
        origin: url,
        browserContextId: (await permission.send('Target.getTargetInfo')).targetInfo.browserContextId,
      })
      const deniedStarted = performance.now()
      await deniedPage.goto(page.url())
      await expect(deniedPage.getByRole('heading', { name: marker, exact: true })).toBeVisible()
      await expect(deniedPage.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'relay')
      await activeStream(deniedPage, 'relay')
      const deniedRead = await deniedPage.evaluate(() => (window as typeof window & { transportObservations: Observation[] }).transportObservations.find(item => item.method === 'GET'))
      expect(deniedRead?.route).toBe('relay')
      evidence.push({ route: deniedRead!.route, operation: 'permission-denied-read', elapsedMs: performance.now() - deniedStarted })
      expect(errors).toEqual([])
      await denied.close()
      await page.bringToFront()

      const beforeCut = (await observations()).length
      const cut = performance.now()
      execFileSync(process.env.CAIRN_NETWORK_CUT_DIRECT!, [], { stdio: 'ignore' })
      const once = 'Exactly once through a transport switch'
      await page.getByLabel('Message', { exact: true }).fill(once)
      await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
      await expect(page.getByText(once, { exact: true })).toHaveCount(1, { timeout: 25000 })
      const resumed = (await observations()).slice(beforeCut)
      expect(resumed.some(item => item.route === 'relay' && item.method === 'STREAM')).toBe(true)
      expect(resumed.some(item => item.route === 'relay' && item.method === 'POST')).toBe(true)
      evidence.push({ route: resumed.find(item => item.method === 'POST')!.route, operation: 'fallback-send', elapsedMs: performance.now() - cut })
      evidence.push({ route: resumed.find(item => item.method === 'STREAM')!.route, operation: 'fallback-stream', elapsedMs: performance.now() - cut })
      const chatId = new URL(page.url()).pathname.split('/').at(-1)!
      const detail = await (await page.request.get(`${url}/api/installations/${installationId}/api/chats/${chatId}`)).json()
      expect(detail.messages.filter((message: { text: string }) => message.text === once)).toHaveLength(1)
      execFileSync(process.env.CAIRN_NETWORK_RESTORE_DIRECT!, [], { stdio: 'ignore' })
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 40000 })
    }

    if (process.env.CAIRN_NETWORK_SCENARIO === 'same-lan') {
      const oldLease = leases.at(-1)
      const leaseCount = leases.length
      await fixture.stop(service)
      service = beacon()
      await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 40000 })
      await expect.poll(() => leases.length, { timeout: 40000 }).toBeGreaterThan(leaseCount)
      expect(leases.at(-1)).not.toBe(oldLease)
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 40000 })
      // Logout below must still revoke the fresh lease signed under the new key.
    }

    // Scoped revocation while browser streams are active, on both delivered routes.
    if (['same-lan', 'udp-blocked'].includes(process.env.CAIRN_NETWORK_SCENARIO!)) {
      const email = `network-member-${Date.now()}@example.test`
      const headers = { 'origin': url, 'x-csrf-token': session.csrf }
      const invite = await page.request.post(`${url}/api/installations/${installationId}/sharing/invitations`, { headers, data: { email } })
      expect(invite.ok()).toBe(true)
      const member = await browser.newPage()
      await capture(member)
      await member.goto(url)
      await member.getByLabel('Email address').fill(email)
      const beforeCode = messages.length
      await member.getByRole('button', { name: 'Send code', exact: true }).click()
      await expect.poll(() => messages.slice(beforeCode).find(message => /\b\d{8}\b/.test(message))).toBeTruthy()
      await member.getByLabel('Email code').fill(messages.slice(beforeCode).find(message => /\b\d{8}\b/.test(message))!.match(/\b\d{8}\b/)![0])
      await member.getByRole('button', { name: 'Sign in', exact: true }).click()
      await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
      await expect(member).toHaveURL(/\/installations\/[^/]+\/$/)
      await expect(member.getByRole('link', { name: 'New conversation', exact: true }).first()).toBeVisible()
      const memberSession = await (await member.request.get(`${url}/api/account/session`)).json()
      const chatId = new URL(page.url()).pathname.split('/').at(-1)!
      await member.goto(page.url())
      await expect(member.getByRole('heading', { name: marker, exact: true })).toBeVisible()
      await expect(member.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', expected, { timeout: 35000 })
      await activeStream(member, expected)
      const removed = await page.request.delete(`${url}/api/installations/${installationId}/sharing/members/${memberSession.account.id}`, { headers })
      expect(removed.ok()).toBe(true)
      await expect(member.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'relay')
      const denied = await member.request.get(`${url}${chatsPath}`)
      expect(denied.status()).toBe(404)
      const memberStreams = await member.evaluate(() => (window as typeof window & { transportObservations: Observation[] }).transportObservations.filter(item => item.method === 'STREAM').length)
      const afterRemoval = 'Owner stream survives member revocation'
      const published = await page.request.post(`${url}/api/installations/${installationId}/api/chats/${chatId}/messages`, { headers, data: { id: randomUUID(), text: afterRemoval } })
      expect(published.ok()).toBe(true)
      const queued = page.getByRole('button', { name: /^\+ \d+ other messages?$/ })
      if (process.env.CAIRN_NETWORK_SCENARIO === 'same-lan') {
        await expect(queued).toBeVisible()
        await queued.click()
      }

      await expect(page.getByText(afterRemoval, { exact: true })).toBeVisible()
      expect(await member.evaluate(() => (window as typeof window & { transportObservations: Observation[] }).transportObservations.filter(item => item.method === 'STREAM').length)).toBe(memberStreams)
      await expect(member.getByText(afterRemoval, { exact: true })).toHaveCount(0)
      await member.close()
      // Logout cuts every tab sharing this session's active streams.
      const device = await browser.newContext({ storageState: await page.context().storageState() })
      const devicePage = await device.newPage()
      await capture(devicePage)
      await devicePage.goto(page.url())
      await expect(devicePage.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', expected, { timeout: 35000 })
      await activeStream(devicePage, expected)
      const logout = await devicePage.request.post(`${url}/api/account/logout`, { headers, data: {} })
      expect(logout.ok()).toBe(true)
      await expect(devicePage.getByLabel('Email address')).toBeVisible({ timeout: 15000 })
      await expect(page.getByLabel('Email address')).toBeVisible({ timeout: 15000 })
      await device.close()
      revocations.push('member-removal', 'logout')
      // A fresh owner session starts a stream before detachment revokes the installation.
      const beforeLogin = messages.length
      await page.getByLabel('Email address').fill(ownerEmail)
      await expect(async () => {
        await page.getByRole('button', { name: 'Send code', exact: true }).click()
        await expect(page.getByLabel('Email code')).toBeVisible()
      }).toPass({ timeout: 70000 })
      await expect.poll(() => messages.slice(beforeLogin).find(message => /\b\d{8}\b/.test(message))).toBeTruthy()
      await page.getByLabel('Email code').fill(messages.slice(beforeLogin).find(message => /\b\d{8}\b/.test(message))!.match(/\b\d{8}\b/)![0])
      await page.getByRole('button', { name: 'Sign in', exact: true }).click()
      await expect(page.getByRole('heading', { name: marker, exact: true })).toBeVisible()
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', expected, { timeout: 35000 })
      await activeStream(page, expected)
      const renewed = await (await page.request.get(`${url}/api/account/session`)).json()
      const detached = await page.request.post(`${url}/api/installations/${installationId}/detach`, { headers: { 'origin': url, 'x-csrf-token': renewed.csrf }, data: {} })
      expect(detached.ok()).toBe(true)
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'relay')
      expect((await page.request.get(`${url}${chatsPath}`)).status()).toBe(404)
      revocations.push('detachment')
    }

    const mdnsCandidates = process.env.CAIRN_NETWORK_SCENARIO === 'mdns-only-client'
      ? await page.evaluate(() => (window as typeof window & { iceCandidates: { mdns: boolean, type: string | null }[] }).iceCandidates)
      : undefined
    if (mdnsCandidates) {
      expect(mdnsCandidates.length).toBeGreaterThan(0)
      expect(mdnsCandidates.every(candidate => candidate.mdns && candidate.type === 'host')).toBe(true)
    }

    phases.enter('report-write')
    const output = process.env.CAIRN_NETWORK_OUTPUT!
    const report = JSON.parse(await readFile(output, 'utf8'))
    const rustReads = evidence.filter(item => item.operation.startsWith('rust-read'))
    const directReads = rustReads.filter(item => item.route === 'direct').length
    const directNegotiations = process.env.CAIRN_NETWORK_SCENARIO === 'packet-loss'
      ? {
          attempts: rustReads.length,
          direct: directReads,
          relay: rustReads.filter(item => item.route === 'relay').length,
          ratio: directReads / rustReads.length,
        }
      : undefined
    await writeFile(output, `${JSON.stringify({
      ...report,
      expectedRoute: process.env.CAIRN_NETWORK_EXPECT_ROUTE || 'relay',
      expectedRustRoute: process.env.CAIRN_NETWORK_EXPECT_RUST_ROUTE,
      observations: evidence,
      directNegotiations,
      qualificationContract: packetLoss ? 'unbounded-stress-routing-measurement' : undefined,
      oldDeliveryPrerequisites: packetLoss ? oldDeliveryPrerequisites : undefined,
      revocations,
      mdnsCandidates,
    })}\n`)

    const packetLossReads = process.env.CAIRN_NETWORK_SCENARIO === 'packet-loss' && process.env.CAIRN_NETWORK_EXPECT_RUST_ROUTE === 'direct'
      ? rustReads
      : []
    if (packetLossReads.length) {
      const saved = JSON.parse(await readFile(output, 'utf8'))
      expect(saved).toHaveProperty('directNegotiations')
      expect(saved.directNegotiations.attempts).toBe(3)
      expect(saved.directNegotiations.direct + saved.directNegotiations.relay).toBe(3)
      expect([0, 1 / 3, 2 / 3, 1]).toContain(saved.directNegotiations.ratio)
      expect(packetLossReads).toHaveLength(3)
      oldDeliveryPrerequisites.atLeastOneRustDirect = packetLossReads.some(item => item.route === 'direct')
      oldDeliveryPrerequisites.allRustFallbacksIce = packetLossReads.every(item => item.route === 'direct' || (item.directFailure?.phase === 'ice' && item.directFailure.timedOut))
      const satisfied = oldDeliveryPrerequisites.promotion && oldDeliveryPrerequisites.browserFallback
        && oldDeliveryPrerequisites.atLeastOneRustDirect && oldDeliveryPrerequisites.allRustFallbacksIce
      await writeFile(output, `${JSON.stringify({
        ...saved,
        oldDeliveryPrerequisites,
        historicalDeliveryOracle: {
          executed: false,
          verdict: satisfied ? 'would-satisfy-delivery-prerequisites' : 'would-fail-delivery-prerequisites',
        },
      })}\n`)
    }

    phases.enter('route-check')
    for (const observation of evidence) {
      if (packetLossReads.includes(observation) && observation.route === 'relay') {
        // A timeout is measured only with proof of the unchanged shared budget.
        // HTTP refusals, SDK errors and unknown early closes remain failures.
        expect(observation.directFailure?.timedOut).toBe(true)
        expect(observation.failureHttpStatus).toBeNull()
        const expiry = observation.directPhases?.find(item => item.event === 'deadline-expired')
        expect(expiry).toMatchObject({ remainingMs: 0 })
        const deadline = observation.directPhases?.find(item => item.event === 'deadline-started')
        expect(deadline).toBeDefined()
        const budgetElapsedMs = expiry!.elapsedMs - deadline!.elapsedMs
        expect(budgetElapsedMs).toBeGreaterThanOrEqual(29900)
        expect(budgetElapsedMs).toBeLessThan(32000)
        continue
      }

      if (packetLoss && observation.operation === 'browser-send' && observation.route === 'relay') {
        expect(['heartbeat-timeout', 'opening-timeout', 'peer-failed']).toContain(observation.fallbackReason)
        continue
      }

      const expectedRoute = observation.operation.startsWith('rust-')
        ? process.env.CAIRN_NETWORK_EXPECT_RUST_ROUTE
        : process.env.CAIRN_NETWORK_EXPECT_ROUTE || 'relay'
      const route = ['bootstrap-read', 'bootstrap-create', 'stream-resume', 'fallback-send', 'fallback-stream', 'permission-denied-read'].includes(observation.operation) ? 'relay' : expectedRoute
      expect(observation.route, `${observation.operation}: actual route`).toBe(route)
    }

    phases.enter('logout-session')
    const finalSession = await (await page.request.get(`${url}/api/account/session`)).json()
    phases.enter('logout')
    const logout = await page.request.post(`${url}/api/account/logout`, { headers: { 'origin': url, 'x-csrf-token': finalSession.csrf || session.csrf }, data: {} })
    expect([200, 204, 401]).toContain(logout.status())
    phases.enter('logged-out-rust-read')
    await rustRequest(chatsPath, cookie, 401)
    phases.enter('complete')
  }
  finally {
    await closeNetworkFixture(phases, {
      browser,
      fixture,
      captureTransport: async () => {
        const output = process.env.CAIRN_NETWORK_OUTPUT
        if (browser && output) {
          const diagnostics = await Promise.all(browser.contexts().flatMap(context => context.pages())
            .map(page => networkTransportDiagnostics(page).catch(() => [])))
          const diagnosticsOutput = output.endsWith('.json') ? `${output.slice(0, -5)}.transport.json` : `${output}.transport.json`
          await writeFile(diagnosticsOutput, `${JSON.stringify(diagnostics)}\n`)
        }
      },
    })
  }
})
