import type { Page } from '@playwright/test'
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import {
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import { config as loadConfig } from '../legacy/server/config'
import { Service as SeedService } from '../legacy/server/service'
import { Store } from '../legacy/server/store'
import { expireAccountProof } from './official-relay-fixture'

test('owner shares an installation and member works without management controls', async ({ page, browser }) => {
  test.setTimeout(120000)
  const root = await mkdtemp(join(tmpdir(), 'leo-sharing-'))
  const children: ChildProcess[] = []
  const diagnostics: string[] = []
  const messages: Array<{ to: string[], text: string }> = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    const message = JSON.parse(body)
    messages.push(message)
    response.writeHead(200, { 'content-type': 'application/json' }).end('{}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const url = 'http://localhost:4397'
  let seed: SeedService | undefined
  const ownerEmail = `owner-${Date.now()}@example.test`
  const memberEmail = `member-${Date.now()}@example.test`
  const memberContext = await browser.newContext()
  const member = await memberContext.newPage()
  const forbidden: string[] = []
  member.on('response', (response) => {
    if (response.status() === 403 && response.url().includes('/api/installations/'))
      forbidden.push(response.url())
  })

  function start(binary: string, env: NodeJS.ProcessEnv) {
    const child = spawn(binary, [], { env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'] })
    child.stdout!.on('data', chunk => diagnostics.push(String(chunk)))
    child.stderr!.on('data', chunk => diagnostics.push(String(chunk)))
    children.push(child)
    return child
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

  const official = start('target/debug/leo-official', {
    LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
    LEO_OFFICIAL_ORIGIN: url,
    LEO_OFFICIAL_LISTEN: '127.0.0.1:4397',
    LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${(mail.address() as { port: number }).port}/emails`,
    LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
    LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
  })
  try {
    await expect.poll(() => {
      if (official.exitCode !== null)
        throw new Error(diagnostics.join('') || `Official service exited: ${official.exitCode}`)
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }, { timeout: 30000 }).toBe(true)
    await signIn(page, ownerEmail)
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    await Promise.all([mkdir(join(root, 'data')), mkdir(join(root, 'home'))])
    seed = new SeedService(new Store(join(root, 'data')), loadConfig({
      dataDir: join(root, 'data'),
      home: join(root, 'home'),
      workspaceRoots: [root],
      workerEnabled: false,
      logger: false,
    }))
    const installation = start('target/debug/leo', {
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
    await expect.poll(async () => {
      if (installation.exitCode !== null)
        throw new Error(diagnostics.join('') || `Installation exited: ${installation.exitCode}`)
      return (await (await page.request.get(`${url}/api/account/session`)).json()).installations.length
    }, { timeout: 30000 }).toBe(1).catch((cause) => {
      throw new Error(`${cause.message}\n${diagnostics.join('')}`)
    })
    await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
    await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Share installation', exact: true }).click()
    await expect(page.getByText('Members use your coding-agent accounts and secrets.', { exact: true })).toBeVisible()
    expireAccountProof(new URL(process.env.LEO_OFFICIAL_TEST_DATABASE_URL!), ownerEmail)
    await page.getByLabel('Invite by email', { exact: true }).fill(memberEmail)
    await page.getByRole('button', { name: 'Send invitation', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Confirm your identity')
    await expect(page.getByRole('button', { name: 'Confirm identity', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Confirm identity', exact: true })).toBeVisible()
    const emailsBeforeConfirmation = messages.length
    await page.getByRole('button', { name: 'Send confirmation code', exact: true }).click()
    await expect.poll(() => messages.slice(emailsBeforeConfirmation).find(message => message.to.includes(ownerEmail) && /\b\d{8}\b/.test(message.text))?.text).toBeTruthy()
    const confirmation = messages.slice(emailsBeforeConfirmation).find(message => message.to.includes(ownerEmail) && /\b\d{8}\b/.test(message.text))!
    await page.getByLabel('Confirmation code').fill(confirmation.text.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Verify confirmation code', exact: true }).click()
    await expect(page.getByLabel('Invite by email', { exact: true })).toHaveValue(memberEmail)
    expect(messages.some(message => message.to.includes(memberEmail))).toBe(false)
    await page.getByRole('button', { name: 'Send invitation', exact: true }).click()
    await expect(page.getByRole('region', { name: 'Installation sharing' }).getByText(memberEmail, { exact: true })).toBeVisible()
    await expect.poll(() => messages.some(message => message.to.includes(memberEmail) && message.text.includes('Shared home'))).toBe(true)
    await signIn(member, memberEmail)
    await expect(member.getByRole('region', { name: 'Pending invitations' }).getByText('Shared home', { exact: true })).toBeVisible()
    await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
    await expect(member).toHaveURL(/\/installations\/[^/]+\/$/)
    const installationUrl = member.url()
    const memberSession = await (await member.request.get(`${url}/api/account/session`)).json()
    const apiRoot = `${url}/api/installations/${memberSession.installations[0].id}/api`
    const agents = await (await member.request.get(`${apiRoot}/agents`)).json()
    const taskResponse = await member.request.post(`${apiRoot}/tasks`, {
      headers: { 'origin': url, 'x-csrf-token': memberSession.csrf },
      data: {
        name: 'Member commitment',
        prompt: 'Shared scheduled work',
        agentId: agents[0].id,
        cron: '0 9 * * *',
        timezone: 'UTC',
        enabled: true,
      },
    })
    expect(taskResponse.status()).toBe(200)
    const task = await taskResponse.json()
    expect(task.authorId).toBe(memberSession.account.id)
    await member.goto(`${installationUrl}tasks?task=${task.id}`)
    await member.getByRole('button', { name: 'Mission actions', exact: true }).click()
    await expect(member.getByRole('menuitem', { name: 'Delete', exact: true })).toHaveCount(0)
    await expect(member.getByRole('menuitem', { name: 'Duplicate (paused)', exact: true })).toBeVisible()
    await member.goto(installationUrl)
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
    await member.getByRole('link', { name: 'Projects', exact: true }).first().click()
    await expect(member.getByRole('button', { name: /New project|Add a project/ })).toHaveCount(0)
    await member.getByRole('link', { name: 'Skills', exact: true }).first().click()
    await expect(member.getByRole('button', { name: /New skill|Create your first skill/ })).toHaveCount(0)
    await member.goto(`${installationUrl}settings`)
    await expect(member.getByRole('heading', { name: 'Settings', exact: true })).toHaveCount(0)
    await member.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await member.getByRole('textbox', { name: 'Message', exact: true }).fill('Shared conversation')
    await member.getByRole('button', { name: 'Send', exact: true }).click()
    await expect(member.getByRole('heading', { name: 'Shared conversation', exact: true })).toBeVisible()
    await member.reload()
    await expect(member.getByRole('heading', { name: 'Shared conversation', exact: true })).toBeVisible()
    const conversationUrl = member.url()
    const runResponse = await member.request.post(`${apiRoot}/tasks/${task.id}/run`, {
      headers: { 'origin': url, 'x-csrf-token': memberSession.csrf },
    })
    expect(runResponse.status()).toBe(200)
    const runId = (await runResponse.json()).id
    const report = {
      id: randomUUID(),
      runId,
      messageId: null,
      key: 'shared-report',
      version: 1,
      title: 'Shared report',
      name: 'report.md',
      kind: 'markdown',
      size: 15,
      mediaType: 'text/plain',
      createdAt: Date.now(),
      url: '',
      group: 'Shared report',
      previewStatus: 'none',
      visibility: 'private',
      excerpt: '# Shared report',
    }
    await mkdir(join(root, 'data', 'artifacts'), { recursive: true })
    await writeFile(join(root, 'data', 'artifacts', report.id), '# Shared report')
    seed.store.set(`artifact:${runId}:${report.id}`, report)
    seed.store.event(runId, 'artifact', report.title, report)
    await member.goto(`${installationUrl}runs/${runId}`)
    await member.getByRole('button', { name: 'Open Shared report', exact: true }).click()
    await member.getByRole('button', { name: 'Share file', exact: true }).click()
    const memberSharing = member.getByRole('region', { name: 'File sharing' })
    await expect(memberSharing.getByRole('button', { name: 'Enable public link', exact: true })).toHaveCount(0)
    await expect(memberSharing.getByText('The installation owner controls public links.', { exact: true })).toBeVisible()
    await member.getByRole('button', { name: 'Close viewer', exact: true }).click()
    await page.goto(`${installationUrl}runs/${runId}`)
    await page.getByRole('button', { name: 'Open Shared report', exact: true }).click()
    await page.getByRole('button', { name: 'Share file', exact: true }).click()
    const ownerSharing = page.getByRole('region', { name: 'File sharing' })
    await ownerSharing.getByRole('button', { name: 'Enable public link', exact: true }).click()
    await expect(ownerSharing.getByRole('textbox', { name: 'Public link', exact: true })).toBeVisible()
    const publicUrl = await ownerSharing.getByRole('textbox', { name: 'Public link', exact: true }).inputValue()
    await member.reload()
    await member.getByRole('button', { name: 'Open Shared report', exact: true }).click()
    await member.getByRole('button', { name: 'Share file', exact: true }).click()
    await expect(memberSharing.getByRole('button', { name: 'Copy public link', exact: true })).toBeVisible()
    await expect(memberSharing.getByRole('button', { name: 'Disable public link', exact: true })).toHaveCount(0)
    await member.getByRole('button', { name: 'Close viewer', exact: true }).click()
    await member.goto(conversationUrl)
    await page.goto(installationUrl)
    expect(forbidden).toEqual([])
    await page.reload()
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Share installation', exact: true }).click()
    await page.getByRole('button', { name: `Remove ${memberEmail}`, exact: true }).click()
    await expect(page.getByRole('region', { name: 'Installation sharing' }).getByText(memberEmail, { exact: true })).toHaveCount(0)
    expect((await page.request.get(publicUrl)).status()).toBe(200)
    await member.reload()
    await expect(member.getByRole('alert')).toContainText('no longer accessible')
    await page.getByLabel('Invite by email', { exact: true }).fill(memberEmail)
    await page.getByRole('button', { name: 'Send invitation', exact: true }).click()
    await expect(page.getByRole('region', { name: 'Installation sharing' }).getByText(memberEmail, { exact: true })).toBeVisible()
    await member.goto(`${url}/?invitations=1`)
    await member.getByRole('button', { name: 'Accept invitation', exact: true }).click()
    await expect(member).toHaveURL(/\/installations\/[^/]+\/$/)
    await member.getByText('Installation options', { exact: true }).click()
    await member.getByRole('button', { name: 'Leave installation', exact: true }).click()
    await member.getByRole('button', { name: 'Confirm leaving', exact: true }).click()
    await expect(member.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.getByRole('button', { name: 'Close sharing', exact: true }).click()
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Share installation', exact: true }).click()
    await expect(page.getByText('No members yet.', { exact: true })).toBeVisible()
    const cancelledEmail = `cancelled-${Date.now()}@example.test`
    await page.getByLabel('Invite by email', { exact: true }).fill(cancelledEmail)
    await page.getByRole('button', { name: 'Send invitation', exact: true }).click()
    await page.getByRole('button', { name: `Cancel invitation to ${cancelledEmail}`, exact: true }).click()
    await expect(page.getByText('No pending invitations.', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Close sharing', exact: true }).click()
    await page.goto(`${installationUrl}tasks?task=${task.id}`)
    await expect(page.getByText('Schedule stopped because its author\'s access ended. Duplicate this mission to schedule it again.', { exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Resume schedule', exact: true })).toBeDisabled()
    await page.getByRole('button', { name: 'Mission actions', exact: true }).click()
    await expect(page.getByRole('menuitem', { name: 'Delete', exact: true })).toBeVisible()
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
    await seed?.accounts.close()
    seed?.store.close()
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})
