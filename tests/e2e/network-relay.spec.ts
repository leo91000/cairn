import type { Browser, Page } from '@playwright/test'
import { execFileSync, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import process from 'node:process'
import { chromium, expect, test } from '@playwright/test'
import { officialRelayFixture } from './official-relay-fixture'

test('authenticated browser and Rust client keep using the observed route under network constraints', async () => {
  test.setTimeout(240000)
  const fixture = await officialRelayFixture(4398, '198.18.103.1', process.env.LEO_NETWORK_FIXTURE_DIRECTORY)
  const {
    root,
    url,
    messages,
    start,
    official,
  } = fixture
  let browser: Browser | undefined
  const evidence: { route: string, operation: string, elapsedMs: number }[] = []
  const revocations: string[] = []

  async function rustRequest(path: string, cookie: string, status: number, marker?: string) {
    const directRead = status === 200 && marker !== undefined
    const executable = directRead ? process.env.LEO_NETWORK_DIRECT_CLIENT! : process.env.LEO_NETWORK_RUST_CLIENT!
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
    let service = official()
    // Exercise numeric ICE paths deterministically; the separate mDNS-only client
    // case preserves Chromium's default privacy behavior and expects the relay.
    browser = await chromium.launch({
      executablePath: process.env.LEO_NETWORK_CHROMIUM,
      args: process.env.LEO_NETWORK_SCENARIO === 'mdns-only-client' ? [] : ['--disable-features=WebRtcHideLocalIpsWithMdns'],
    })
    const page = await browser.newPage()
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
    }

    async function capture(targetPage: Page) {
      await targetPage.addInitScript(() => {
        const target = window as typeof window & { transportObservations: unknown[] }
        target.transportObservations = []
        window.addEventListener('leo-transport-observation', (event) => {
          target.transportObservations.push((event as CustomEvent).detail)
          if (target.transportObservations.length > 200)
            target.transportObservations.shift()
        })
      })
    }

    await capture(page)
    if (process.env.LEO_NETWORK_SCENARIO === 'mdns-only-client') {
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
    const expected = process.env.LEO_NETWORK_EXPECT_ROUTE || 'relay'
    let releaseDirect: () => void = () => {}
    const authorizationGate = new Promise<void>(resolve => releaseDirect = resolve)
    await page.route('**/direct/authorize', async (route) => {
      await authorizationGate
      await route.continue()
    })
    await expect.poll(() => {
      expect(service.exitCode, 'official binary must remain running').toBeNull()
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }).toBe(true)
    await page.goto(url)
    const ownerEmail = `network-${process.env.LEO_NETWORK_SCENARIO}-${Date.now()}@example.test`
    await page.getByLabel('Email address').fill(ownerEmail)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    const installation = start('target/debug/leo', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      HOST: '127.0.0.1',
      PORT: '4399',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: code,
      LEO_INSTALLATION_NAME: 'Network bench installation',
    })
    const bootstrapStart = performance.now()
    await expect(async () => {
      expect(installation.exitCode).toBeNull()
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass({ timeout: 30000 })
    await expect(page.getByRole('link', { name: 'New conversation', exact: true }).first()).toBeVisible()
    await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'relay')
    await expect.poll(async () => (await observations()).some(item => item.route === 'relay' && item.method === 'GET')).toBe(true)
    evidence.push({ route: (await observations()).find(item => item.method === 'GET')!.route, operation: 'bootstrap-read', elapsedMs: performance.now() - bootstrapStart })
    releaseDirect()
    const installationId = new URL(page.url()).pathname.split('/')[2]!
    const chatsPath = `/api/installations/${installationId}/api/chats`
    const marker = `Network scenario ${process.env.LEO_NETWORK_SCENARIO}`
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByLabel('Message', { exact: true }).fill(marker)
    await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', expected, { timeout: 35000 })
    const before = (await observations()).length
    const started = performance.now()
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    await expect.poll(async () => (await observations()).slice(before).some(item => item.method === 'POST' && item.path.endsWith('/messages'))).toBe(true)
    await expect(page.getByRole('heading', { name: marker, exact: true })).toBeVisible()
    const sent = (await observations()).slice(before).find(item => item.method === 'POST' && item.path.endsWith('/messages'))!
    evidence.push({ route: sent.route, operation: 'browser-send', elapsedMs: performance.now() - started })
    const cookies = await page.context().cookies()
    const cookie = cookies.map(value => `${value.name}=${value.value}`).join('; ')
    evidence.push({ ...await rustRequest(chatsPath, cookie, 200, marker), operation: 'rust-read' })
    await rustRequest(chatsPath, '', 401)
    // A loopback listener on the installation still refuses anonymous local access.
    const local = execFileSync(process.env.LEO_NETWORK_LOCAL_CLIENT!, ['http', '127.0.0.1:4399', '/api/chats', '401'], { input: '', encoding: 'utf8' })
    expect(JSON.parse(local).status).toBe(401)
    const session = await (await page.request.get(`${url}/api/account/session`)).json()

    if (process.env.LEO_NETWORK_SCENARIO === 'network-change') {
      const beforeChange = (await observations()).length
      const changed = performance.now()
      execFileSync(process.env.LEO_NETWORK_CHANGE!, [], { stdio: 'ignore' })
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

    if (process.env.LEO_NETWORK_SCENARIO === 'same-lan') {
      const beforeCut = (await observations()).length
      const cut = performance.now()
      execFileSync(process.env.LEO_NETWORK_CUT_DIRECT!, [], { stdio: 'ignore' })
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
      execFileSync(process.env.LEO_NETWORK_RESTORE_DIRECT!, [], { stdio: 'ignore' })
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 40000 })
    }

    if (process.env.LEO_NETWORK_SCENARIO === 'same-lan') {
      const oldLease = leases.at(-1)
      const leaseCount = leases.length
      await fixture.stop(service)
      service = official()
      await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 40000 })
      await expect.poll(() => leases.length, { timeout: 40000 }).toBeGreaterThan(leaseCount)
      expect(leases.at(-1)).not.toBe(oldLease)
      await expect(page.getByRole('status', { name: 'Connection route' })).toHaveAttribute('data-transport-route', 'direct', { timeout: 40000 })
      // Logout below must still revoke the fresh lease signed under the new key.
    }

    // Scoped revocation while browser streams are active, on both delivered routes.
    if (['same-lan', 'udp-blocked'].includes(process.env.LEO_NETWORK_SCENARIO!)) {
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
      const memberSession = await (await member.request.get(`${url}/api/account/session`)).json()
      const chatId = new URL(page.url()).pathname.split('/').at(-1)!
      await member.goto(`${url}/installations/${installationId}/chats/${chatId}`)
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
      if (process.env.LEO_NETWORK_SCENARIO === 'same-lan') {
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

    const mdnsCandidates = process.env.LEO_NETWORK_SCENARIO === 'mdns-only-client'
      ? await page.evaluate(() => (window as typeof window & { iceCandidates: { mdns: boolean, type: string | null }[] }).iceCandidates)
      : undefined
    if (mdnsCandidates) {
      expect(mdnsCandidates.length).toBeGreaterThan(0)
      expect(mdnsCandidates.every(candidate => candidate.mdns && candidate.type === 'host')).toBe(true)
    }

    const output = process.env.LEO_NETWORK_OUTPUT!
    const report = JSON.parse(await readFile(output, 'utf8'))
    await writeFile(output, `${JSON.stringify({
      ...report,
      expectedRoute: process.env.LEO_NETWORK_EXPECT_ROUTE || 'relay',
      expectedRustRoute: process.env.LEO_NETWORK_EXPECT_RUST_ROUTE,
      observations: evidence,
      revocations,
      mdnsCandidates,
    })}\n`)

    for (const observation of evidence) {
      const expectedRoute = observation.operation.startsWith('rust-')
        ? process.env.LEO_NETWORK_EXPECT_RUST_ROUTE
        : process.env.LEO_NETWORK_EXPECT_ROUTE || 'relay'
      const route = ['bootstrap-read', 'stream-resume', 'fallback-send', 'fallback-stream'].includes(observation.operation) ? 'relay' : expectedRoute
      expect(observation.route, `${observation.operation}: actual route`).toBe(route)
    }

    const finalSession = await (await page.request.get(`${url}/api/account/session`)).json()
    const logout = await page.request.post(`${url}/api/account/logout`, { headers: { 'origin': url, 'x-csrf-token': finalSession.csrf || session.csrf }, data: {} })
    expect([200, 204, 401]).toContain(logout.status())
    await rustRequest(chatsPath, cookie, 401)
  }
  finally {
    await browser?.close()
    await fixture.close()
  }
})
