import type { ChildProcess } from 'node:child_process'
import { Buffer } from 'node:buffer'
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

test('selects installations, remembers the last one and honours deep workspace URLs', async ({ page }) => {
  test.setTimeout(120000)
  const root = await mkdtemp(join(tmpdir(), 'leo-selector-'))
  const children: ChildProcess[] = []
  const messages: string[] = []
  let installationLog = ''
  await Promise.all([mkdir(join(root, 'data')), mkdir(join(root, 'home'))])
  const seed = new SeedService(new Store(join(root, 'data')), loadConfig({
    dataDir: join(root, 'data'),
    home: join(root, 'home'),
    workspaceRoots: [root],
    workerEnabled: false,
    logger: false,
  }))
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
    await expect(page.getByLabel('Installation command', { exact: true })).toHaveValue(`curl -fsSL '${url}/install.sh' | sudo bash -s -- --claim-code '${code}'`)
    expect((await page.request.get(`${url}/install.sh`)).status()).toBe(200)
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
      LEO_NODE_IMAGE: `registry.example/leo@sha256:${'1'.repeat(64)}`,
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
    // Node administration travels through the official relay; the machine itself
    // needs a separately configured direct manager address, even on a LAN or VPN.
    await page.getByRole('link', { name: 'Nodes', exact: true }).click()
    await page.getByRole('button', { name: 'Add a machine', exact: true }).click()
    const nodeDialog = page.getByRole('dialog', { name: 'Add a machine' })
    await expect(nodeDialog).toContainText('does not use the relay')
    await expect(nodeDialog).toContainText('Disk blocks never pass through the relay')
    await nodeDialog.getByLabel('Machine name').fill('VPN node')
    await nodeDialog.getByLabel('Direct manager address').fill('https://manager.vpn.example:4310')
    await nodeDialog.getByRole('button', { name: 'Create enrollment code' }).click()
    await expect(nodeDialog.getByLabel('Node installation command')).toContainText('https://manager.vpn.example:4310/internal/nodes/install.sh?managerUrl=')
    await expect(nodeDialog.getByLabel('Node installation command')).not.toContainText(url)
    await expect(nodeDialog.getByLabel('Single-use enrollment code')).toHaveText(/^[\w-]{43}$/)
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 900 })
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    }

    await page.setViewportSize({ width: 1440, height: 900 })
    await page.keyboard.press('Escape')
    await page.getByRole('link', { name: 'Agents', exact: true }).click()
    const firstUrl = page.url()
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const secondCode = await page.getByLabel('Installation claim code').inputValue()
    await expect(page.getByLabel('Installation command', { exact: true })).toHaveValue(`curl -fsSL '${url}/install.sh' | sudo bash -s -- --claim-code '${secondCode}'`)
    await Promise.all([mkdir(join(root, 'office-data')), mkdir(join(root, 'office-home'))])
    start('target/debug/leo', {
      DATA_DIR: join(root, 'office-data'),
      AGENT_HOME: join(root, 'office-home'),
      WORKSPACE_ROOTS: root,
      WORKER_ENABLED: 'false',
      NODE_ENV: 'test',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: secondCode,
      LEO_INSTALLATION_NAME: 'Office',
    })
    await expect.poll(async () => {
      const session = await (await page.request.get(`${url}/api/account/session`)).json()
      return session.installations.length
    }, { timeout: 30000 }).toBe(2)
    await page.reload()
    const selector = page.getByRole('combobox', { name: 'Current installation' })
    await expect(selector).toBeVisible()
    await selector.selectOption({ label: 'Office · Online' })
    await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    const officeUrl = page.url()
    expect(officeUrl).not.toBe(firstUrl.replace(/agents$/, ''))
    await page.goto(url)
    await expect(page).toHaveURL(officeUrl)
    await expect(selector).toHaveValue(officeUrl.split('/')[4]!)
    await page.goto(firstUrl)
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toBeVisible()
    await expect(selector.locator('option:checked')).toHaveText('Home · Online')
    await page.goto(url)
    await expect(page).toHaveURL(firstUrl.replace(/agents$/, ''))
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Rename installation', exact: true }).click({ timeout: 7000 })
    await page.getByLabel('Installation name', { exact: true }).fill('My home')
    await page.getByRole('button', { name: 'Save installation name', exact: true }).click()
    await expect(selector.locator('option:checked')).toHaveText('My home · Online')
    await page.reload()
    await expect(selector.locator('option:checked')).toHaveText('My home · Online')
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('A conversation at home')
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await expect(page).toHaveURL(/\/installations\/[^/]+\/chats\/[^/]+$/)
    await expect(page.getByRole('heading', { name: 'A conversation at home', exact: true })).toBeVisible()
    const conversationUrl = page.url()
    await page.reload()
    await expect(page.getByRole('heading', { name: 'A conversation at home', exact: true })).toBeVisible()
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('A home-only draft')
    await selector.selectOption({ label: 'Office · Online' })
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('')
    await expect(page.getByRole('link', { name: /A conversation at home/ })).toHaveCount(0)
    await page.goto(conversationUrl)
    await expect(page.getByRole('heading', { name: 'A conversation at home', exact: true })).toBeVisible()
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('A home-only draft')
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Only Home’s new conversation')
    await selector.selectOption({ label: 'Office · Online' })
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('')
    await page.goto(`${firstUrl.replace(/agents$/, '')}chats`)
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Only Home’s new conversation')
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('A file in my home installation')
    await page.getByLabel('Attach files').setInputFiles({ name: 'hello.txt', mimeType: 'text/plain', buffer: Buffer.from('A relayed file') })
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    const download = page.getByRole('link', { name: 'Download hello.txt', exact: true }).first()
    await expect(download).toHaveAttribute('href', /\/api\/installations\/[^/]+\/api\/chats\/[^/]+\/attachments\//)
    const file = await page.request.get(new URL((await download.getAttribute('href'))!, url).href)
    expect(file.status()).toBe(200)
    expect(await file.text()).toBe('A relayed file')
    const chat = seed.store.list('chats').find(item => page.url().endsWith(item.id))!
    const task = seed.task({ name: 'Relayed files', prompt: 'Publish notes', agentId: chat.agentId })
    const run = await seed.enqueue(task.id)
    seed.store.updateRun(run.id, { status: 'succeeded', finishedAt: Date.now() })
    const artifact = {
      id: randomUUID(),
      runId: run.id,
      messageId: null,
      key: 'notes',
      version: 1,
      title: 'Relayed notes',
      name: 'notes.md',
      kind: 'markdown',
      mediaType: 'text/markdown',
      size: 23,
      createdAt: Date.now(),
      url: '',
      group: '',
      previewStatus: 'none',
    }
    await mkdir(join(root, 'data', 'artifacts'), { recursive: true })
    await writeFile(join(root, 'data', 'artifacts', artifact.id), '# Notes from my machine')
    seed.store.set(`artifact:${run.id}:${artifact.id}`, artifact)
    seed.store.event(run.id, 'item.completed', 'Notes', {
      item: {
        id: 'notes-reply',
        type: 'agent_message',
        text: `[Read the notes](/api/runs/${run.id}/artifacts/${artifact.id})`,
      },
    })
    seed.store.put('chats', { ...chat, runId: run.id })
    await page.reload()
    const notesLink = page.getByRole('link', { name: 'Read the notes', exact: true })
    await expect(notesLink).toHaveAttribute('href', /\/api\/installations\/[^/]+\/api\/runs\//)
    await notesLink.click()
    await expect(page.getByRole('heading', { name: 'Notes from my machine', exact: true })).toBeVisible()
    const notesDownload = page.getByRole('link', { name: 'Download original', exact: true })
    await expect(notesDownload).toHaveAttribute('href', /\/api\/installations\/[^/]+\/api\/runs\//)
    const notesResponse = await page.request.get(new URL((await notesDownload.getAttribute('href'))!, url).href)
    expect(notesResponse.status()).toBe(200)
    expect(await notesResponse.text()).toBe('# Notes from my machine')
    await page.getByRole('button', { name: 'Close viewer', exact: true }).click()
    await page.getByRole('link', { name: 'Atelier', exact: true }).first().click()
    await page.getByRole('link', { name: 'Agents', exact: true }).click()
    await page.getByRole('button', { name: 'Edit Main agent', exact: true }).click()
    const portrait = page.getByRole('region', { name: 'Agent portrait' })
    await portrait.getByLabel('Upload agent portrait').setInputFiles('tests/fixtures/artifacts/thumbnail.png')
    await expect(portrait.locator('img')).toHaveJSProperty('naturalWidth', 256)
    await expect(portrait.locator('img')).toHaveAttribute('src', /\/api\/installations\/[^/]+\/api\/agents\//)
    await page.getByRole('button', { name: 'Save agent', exact: true }).click()
    await page.getByRole('link', { name: 'Settings', exact: true }).click()
    await expect(page).toHaveURL(/\/installations\/[^/]+\/settings$/)
    await expect(page.getByRole('heading', { name: 'Settings', exact: true })).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Conversation storage', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Configure external S3', exact: true }).click()
    await expect(page.getByLabel('S3 endpoint', { exact: true })).toBeVisible()
    await page.getByLabel('S3 endpoint', { exact: true }).fill('https://11111111111111111111111111111111.r2.cloudflarestorage.com')
    const privacy = page.getByRole('checkbox', { name: /I confirm R2 public domains/ })
    await expect(privacy).not.toBeChecked()
    await privacy.check()
    await page.getByLabel('S3 bucket', { exact: true }).fill('another-private-bucket')
    await expect(privacy).not.toBeChecked()
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 900 })
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      expect((await page.getByRole('banner', { name: 'Current installation', exact: true }).boundingBox())!.height).toBeLessThan(80)
      await page.screenshot({ path: test.info().outputPath(`installation-selector-${width}.png`) })
    }

    await page.setViewportSize({ width: 1440, height: 900 })
    await page.goto(`${url}/installations/${randomUUID()}/agents`)
    await expect(page.getByRole('alert')).toContainText('unavailable or no longer accessible')
    await expect(page.getByRole('heading', { name: 'Agents', exact: true })).toHaveCount(0)
    await page.goto(firstUrl)
    await expect(selector.locator('option:checked')).toHaveText('My home · Online')
    await page.getByRole('navigation', { name: 'Workspace navigation', exact: true }).getByRole('button', { name: 'Sign out', exact: true }).click()
    await expect(page).toHaveURL(`${url}/`)
    await expect(page.getByLabel('Email address')).toBeVisible()
    await page.reload()
    await expect(page.getByLabel('Email address')).toBeVisible()
  }
  finally {
    await Promise.all(children.map(async (child) => {
      if (child.exitCode !== null || child.signalCode !== null)
        return
      const exited = once(child, 'exit')
      child.kill('SIGTERM')
      await exited
    }))
    await seed.accounts.close()
    seed.store.close()
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})
