import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('selects installations, remembers the last one and honours deep workspace URLs', async ({ page }) => {
  test.setTimeout(120000)
  const root = await mkdtemp(join(tmpdir(), 'leo-selector-'))
  const children: ChildProcess[] = []
  const messages: string[] = []
  let installationLog = ''
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body).text)
    response.writeHead(200, { 'content-type': 'application/json' }).end('{}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const mailPort = (mail.address() as { port: number }).port
  const url = 'http://localhost:4396'

  function start(binary: string, env: NodeJS.ProcessEnv) {
    const child = spawn(binary, [], { env: { ...process.env, ...env }, stdio: ['ignore', 'ignore', 'pipe'] })
    child.stderr?.on('data', chunk => installationLog += chunk)
    children.push(child)
    return child
  }

  start('target/debug/leo-official', {
    LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
    LEO_OFFICIAL_ORIGIN: url,
    LEO_OFFICIAL_LISTEN: '127.0.0.1:4396',
    LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${mailPort}/emails`,
    LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
    LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
  })
  try {
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await page.goto(url)
    await page.getByLabel('Email address').fill(`selector-${Date.now()}@example.test`)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    await Promise.all([mkdir(join(root, 'data')), mkdir(join(root, 'home'))])
    start('target/debug/leo', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      WORKER_ENABLED: 'false',
      NODE_ENV: 'test',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: code,
      LEO_INSTALLATION_NAME: 'Home',
    })
    await expect.poll(async () => {
      const session = await (await page.request.get(`${url}/api/account/session`)).json()
      return session.installations.length
    }, { message: installationLog, timeout: 30000 }).toBe(1)
    await expect(async () => {
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass({ timeout: 15000 })
    await expect(page.getByRole('navigation', { name: 'Workspace navigation', exact: true })).toBeVisible()
    await expect(page.getByRole('combobox', { name: 'Current installation' })).toHaveCount(0)
    await page.getByRole('link', { name: 'Atelier', exact: true }).first().click()
    await page.getByRole('link', { name: 'Agents', exact: true }).click()
    await expect(page).toHaveURL(/\/installations\/[^/]+\/agents$/)
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
    await page.reload()
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
    const firstUrl = page.url()
    const account = await (await page.request.get(`${url}/api/account/session`)).json()
    const claim = await (await page.request.post(`${url}/api/installations/claim-code`, {
      headers: { 'origin': url, 'x-csrf-token': account.csrf },
      data: {},
    })).json()
    await Promise.all([mkdir(join(root, 'office-data')), mkdir(join(root, 'office-home'))])
    start('target/debug/leo', {
      DATA_DIR: join(root, 'office-data'),
      AGENT_HOME: join(root, 'office-home'),
      WORKSPACE_ROOTS: root,
      WORKER_ENABLED: 'false',
      NODE_ENV: 'test',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: claim.code,
      LEO_INSTALLATION_NAME: 'Office',
    })
    await expect.poll(async () => {
      const session = await (await page.request.get(`${url}/api/account/session`)).json()
      return session.installations.length
    }, { timeout: 30000 }).toBe(2)
    await page.reload()
    const selector = page.getByRole('combobox', { name: 'Current installation' })
    await expect(selector).toBeVisible()
    await selector.selectOption({ label: 'Office' })
    await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    const officeUrl = page.url()
    expect(officeUrl).not.toBe(firstUrl.replace(/agents$/, ''))
    await page.goto(url)
    await expect(page).toHaveURL(officeUrl)
    await expect(selector).toHaveValue(officeUrl.split('/')[4]!)
    await page.goto(firstUrl)
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
    await expect(selector.locator('option:checked')).toHaveText('Home')
    await page.goto(url)
    await expect(page).toHaveURL(firstUrl.replace(/agents$/, ''))
    await page.getByRole('button', { name: 'Rename installation', exact: true }).click({ timeout: 7000 })
    await page.getByLabel('Installation name', { exact: true }).fill('My home')
    await page.getByRole('button', { name: 'Save installation name', exact: true }).click()
    await expect(selector.locator('option:checked')).toHaveText('My home')
    await page.reload()
    await expect(selector.locator('option:checked')).toHaveText('My home')
  }
  finally {
    await Promise.all(children.map(async (child) => {
      if (child.exitCode !== null || child.signalCode !== null)
        return
      const exited = once(child, 'exit')
      child.kill('SIGTERM')
      await exited
    }))
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})
