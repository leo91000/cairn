import type { Browser } from '@playwright/test'
import { execFileSync, spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { readFile, writeFile } from 'node:fs/promises'
import { join } from 'node:path'
import process from 'node:process'
import { chromium, expect, test } from '@playwright/test'
import { officialRelayFixture } from './official-relay-fixture'

test('authenticated browser and Rust client keep using the observed route under network constraints', async () => {
  test.setTimeout(120000)
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
    const service = official()
    browser = await chromium.launch({ executablePath: process.env.LEO_NETWORK_CHROMIUM })
    const page = await browser.newPage()
    await expect.poll(() => {
      expect(service.exitCode, 'official binary must remain running').toBeNull()
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }).toBe(true)
    await page.goto(url)
    await page.getByLabel('Email address').fill(`network-${process.env.LEO_NETWORK_SCENARIO}-${Date.now()}@example.test`)
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
    await expect(async () => {
      expect(installation.exitCode).toBeNull()
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass({ timeout: 30000 })
    const installationId = new URL(page.url()).pathname.split('/')[2]!
    const chatsPath = `/api/installations/${installationId}/api/chats`
    const marker = `Network scenario ${process.env.LEO_NETWORK_SCENARIO}`
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByLabel('Message', { exact: true }).fill(marker)
    const sent = page.waitForResponse(response => response.url().includes(`/api/installations/${installationId}/api/`) && response.request().method() === 'POST' && response.url().endsWith('/messages'))
    const started = performance.now()
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    const sentResponse = await sent
    expect(sentResponse.status()).toBe(200)
    await expect(page.getByRole('heading', { name: marker, exact: true })).toBeVisible()
    // The real browser POST went through the official installation relay endpoint.
    evidence.push({ route: (await sentResponse.headerValue('x-leo-transport'))!, operation: 'browser-send', elapsedMs: performance.now() - started })
    const cookies = await page.context().cookies()
    const cookie = cookies.map(value => `${value.name}=${value.value}`).join('; ')
    evidence.push({ ...await rustRequest(chatsPath, cookie, 200, marker), operation: 'rust-read' })
    await rustRequest(chatsPath, '', 401)
    // A loopback listener on the installation still refuses anonymous local access.
    const local = execFileSync(process.env.LEO_NETWORK_LOCAL_CLIENT!, ['http', '127.0.0.1:4399', '/api/chats', '401'], { input: '', encoding: 'utf8' })
    expect(JSON.parse(local).status).toBe(401)
    const session = await (await page.request.get(`${url}/api/account/session`)).json()

    if (process.env.LEO_NETWORK_SCENARIO === 'network-change') {
      const resumed = page.waitForResponse(response => response.url().includes(`/api/installations/${installationId}/api/`) && /\/stream\?/.test(response.url()) && response.status() === 200, { timeout: 30000 })
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
      const resumedResponse = await resumed
      await expect(page.getByText(fresh, { exact: true })).toBeVisible({ timeout: 30000 })
      evidence.push({ route: (await resumedResponse.headerValue('x-leo-transport'))!, operation: 'stream-resume', elapsedMs: performance.now() - changed })
      expect(evidence.at(-1)!.elapsedMs).toBeLessThan(30000)
      evidence.push({ ...await rustRequest(chatsPath, cookie, 200, marker), operation: 'rust-after-change' })
      await expect(page.getByRole('heading', { name: marker, exact: true })).toBeVisible()
    }

    const output = process.env.LEO_NETWORK_OUTPUT!
    const report = JSON.parse(await readFile(output, 'utf8'))
    await writeFile(output, `${JSON.stringify({
      ...report,
      expectedRoute: process.env.LEO_NETWORK_EXPECT_ROUTE || 'relay',
      expectedRustRoute: process.env.LEO_NETWORK_EXPECT_RUST_ROUTE,
      observations: evidence,
    })}\n`)

    for (const observation of evidence) {
      const expectedRoute = observation.operation.startsWith('rust-')
        ? process.env.LEO_NETWORK_EXPECT_RUST_ROUTE
        : process.env.LEO_NETWORK_EXPECT_ROUTE || 'relay'
      expect(observation.route, `${observation.operation}: actual route`).toBe(expectedRoute)
    }

    const logout = await page.request.post(`${url}/api/account/logout`, { headers: { 'origin': url, 'x-csrf-token': session.csrf }, data: {} })
    expect(logout.ok()).toBe(true)
    await rustRequest(chatsPath, cookie, 401)
  }
  finally {
    await browser?.close()
    await fixture.close()
  }
})
