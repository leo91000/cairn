import type { Page } from '@playwright/test'
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('owner shares an installation and member works without management controls', async ({ page, browser }) => {
  test.setTimeout(120000)
  const root = await mkdtemp(join(tmpdir(), 'leo-sharing-'))
  const children: ChildProcess[] = []
  const messages: Array<{ to: string[], text: string }> = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body))
    response.writeHead(200, { 'content-type': 'application/json' }).end('{}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const url = 'http://localhost:4395'
  const memberEmail = `member-${Date.now()}@example.test`
  const memberContext = await browser.newContext()
  const member = await memberContext.newPage()

  function start(binary: string, env: NodeJS.ProcessEnv) {
    const child = spawn(binary, [], { env: { ...process.env, ...env }, stdio: 'ignore' })
    children.push(child)
  }

  async function signIn(target: Page, email: string) {
    await target.goto(url)
    await target.getByLabel('Email address').fill(email)
    await target.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.findLast(message => message.to.includes(email) && /\b\d{8}\b/.test(message.text))?.text).toBeTruthy()
    const message = messages.findLast(message => message.to.includes(email) && /\b\d{8}\b/.test(message.text))!
    await target.getByLabel('Email code').fill(message.text.match(/\b\d{8}\b/)![0])
    await target.getByRole('button', { name: 'Sign in', exact: true }).click()
  }

  start('target/debug/leo-official', {
    LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
    LEO_OFFICIAL_ORIGIN: url,
    LEO_OFFICIAL_LISTEN: '127.0.0.1:4395',
    LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${(mail.address() as { port: number }).port}/emails`,
    LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
    LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
  })
  try {
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await signIn(page, `owner-${Date.now()}@example.test`)
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
      LEO_INSTALLATION_NAME: 'Shared home',
    })
    await expect.poll(async () => (await (await page.request.get(`${url}/api/account/session`)).json()).installations.length, { timeout: 30000 }).toBe(1)
    await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
    await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Share installation', exact: true }).click()
    await expect(page.getByText('Members use your coding-agent accounts and secrets.', { exact: true })).toBeVisible()
    await page.getByLabel('Invite by email', { exact: true }).fill(memberEmail)
    await page.getByRole('button', { name: 'Send invitation', exact: true }).click()
    await expect(page.getByRole('region', { name: 'Installation sharing' }).getByText(memberEmail, { exact: true })).toBeVisible()
    await expect.poll(() => messages.some(message => message.to.includes(memberEmail) && message.text.includes('Shared home'))).toBe(true)
    await signIn(member, memberEmail)
    await expect(member.getByRole('region', { name: 'Pending invitations' }).getByText('Shared home', { exact: true })).toBeVisible()
    await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
    await expect(member).toHaveURL(/\/installations\/[^/]+\/$/)
    const installationUrl = member.url()
    await member.getByText('Installation options', { exact: true }).click()
    await expect(member.getByRole('button', { name: 'Rename installation', exact: true })).toHaveCount(0)
    await expect(member.getByRole('button', { name: 'Share installation', exact: true })).toHaveCount(0)
    await expect(member.getByRole('button', { name: 'Leave installation', exact: true })).toBeVisible()
    await member.getByText('Installation options', { exact: true }).click()
    await member.getByRole('link', { name: 'Atelier', exact: true }).first().click()
    for (const name of ['Connections', 'Nodes', 'Settings', 'MCPs'])
      await expect(member.getByRole('link', { name, exact: true })).toHaveCount(0)
    await member.getByRole('link', { name: 'Agents', exact: true }).click()
    await expect(member.getByRole('button', { name: /New agent|Edit Main agent|Delete/ })).toHaveCount(0)
    await expect(member.getByRole('link', { name: 'Start chat', exact: true }).first()).toBeVisible()
    await member.goto(`${installationUrl}settings`)
    await expect(member.getByRole('heading', { name: 'Settings', exact: true })).toHaveCount(0)
    await member.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await member.getByRole('textbox', { name: 'Message', exact: true }).fill('Shared conversation')
    await member.getByRole('button', { name: 'Send', exact: true }).click()
    await expect(member.getByRole('heading', { name: 'Shared conversation', exact: true })).toBeVisible()
    await member.reload()
    await expect(member.getByRole('heading', { name: 'Shared conversation', exact: true })).toBeVisible()
    await page.reload()
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Share installation', exact: true }).click()
    await page.getByRole('button', { name: `Remove ${memberEmail}`, exact: true }).click()
    await member.reload()
    await expect(member.getByRole('alert')).toContainText('no longer accessible')
  }
  finally {
    await memberContext.close()
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
