import type { ChildProcess } from 'node:child_process'
import { Buffer } from 'node:buffer'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import { config as loadConfig } from '../legacy/server/config'
import { Service as SeedService } from '../legacy/server/service'
import { Store } from '../legacy/server/store'

test('claims an installation, reads conversations and sends through the official relay after restart', async ({ page }) => {
  test.setTimeout(60000)
  const messages: string[] = []
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
  const root = await mkdtemp(join(tmpdir(), 'leo-official-relay-'))
  await Promise.all([mkdir(join(root, 'data')), mkdir(join(root, 'home'))])
  const url = 'http://127.0.0.1:4399'
  const children: ChildProcess[] = []
  let hostilePeer: WebSocket | undefined
  // As in the native browser fixtures, open the seeding module before the
  // native process applies newer database migrations.
  const seed = new SeedService(new Store(join(root, 'data')), loadConfig({
    dataDir: join(root, 'data'),
    home: join(root, 'home'),
    workspaceRoots: [root],
    workerEnabled: false,
    logger: false,
  }))

  function start(binary: string, env: NodeJS.ProcessEnv) {
    const child = spawn(binary, [], { env: { ...process.env, ...env }, stdio: ['ignore', 'ignore', 'ignore'] })
    children.push(child)
    return child
  }

  async function stop(child: ChildProcess) {
    if (child.exitCode !== null || child.signalCode !== null)
      return
    const exited = once(child, 'exit')
    child.kill('SIGTERM')
    await exited
  }

  function official() {
    return start('target/debug/leo-official', {
      LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
      LEO_OFFICIAL_ORIGIN: url,
      LEO_OFFICIAL_LISTEN: '127.0.0.1:4399',
      LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${mailPort}/emails`,
      LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
      LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
    })
  }

  const service = official()
  try {
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await page.goto(url)
    await page.getByLabel('Email address').fill(`relay-${Date.now()}@example.test`)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    expect(code.length).toBeGreaterThanOrEqual(32)
    expect(await page.getByText('This code expires in 10 minutes.')).toBeVisible()
    const installation = start('target/debug/leo', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: code,
      LEO_INSTALLATION_NAME: 'Browser installation',
    })
    await expect(async () => {
      expect(installation.exitCode, 'installation must remain running').toBeNull()
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page.getByRole('button', { name: 'Browser installation', exact: true })).toBeVisible()
    }).toPass()
    await page.getByRole('button', { name: 'Browser installation', exact: true }).click()
    await page.getByRole('button', { name: 'New conversation', exact: true }).click()
    await page.getByLabel('Message', { exact: true }).fill('A message through the relay')
    await page.getByRole('button', { name: 'Send message', exact: true }).click()
    await expect(page.getByRole('list', { name: 'Pending messages' })).toContainText('A message through the relay')
    await page.getByRole('button', { name: 'Back to conversations', exact: true }).click()
    await page.getByRole('button', { name: 'A message through the relay', exact: true }).click()
    await expect(page.getByRole('list', { name: 'Pending messages' })).toContainText('A message through the relay')
    // A process restart really closes every relay socket, unlike stopping a listener.
    await stop(service)
    official()
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await expect(async () => {
      await page.getByRole('button', { name: 'Refresh conversation', exact: true }).click()
      await expect(page.getByRole('alert')).toHaveCount(0)
      await expect(page.getByRole('list', { name: 'Pending messages' })).toContainText('A message through the relay')
    }).toPass({ timeout: 15000 })
    await stop(installation)
    start('target/debug/leo', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      PORT: '0',
    })
    await expect(async () => {
      await page.getByRole('button', { name: 'Refresh conversation', exact: true }).click()
      await expect(page.getByRole('alert')).toHaveCount(0)
    }).toPass({ timeout: 15000 })
    await page.getByLabel('Message', { exact: true }).fill('After reconnection')
    await page.getByRole('button', { name: 'Send message', exact: true }).click()
    await expect(page.getByRole('list', { name: 'Pending messages' })).toContainText('After reconnection')
    // Seed a finished run using the same fixture module as the native browser
    // journeys. Its detail is then read through the real official HTTP relay.
    const chat = seed.store.list('chats')[0]!
    const task = seed.task({ name: 'Finished conversation', prompt: 'Review the workspace', agentId: chat.agentId })
    const run = await seed.enqueue(task.id)
    seed.store.updateRun(run.id, { status: 'succeeded', summary: 'The **relayed agent reply** remains readable.', finishedAt: Date.now() })
    seed.store.put('chats', { ...chat, runId: run.id })
    await page.getByRole('button', { name: 'Refresh conversation', exact: true }).click()
    await expect(page.getByRole('region', { name: 'Agent response' })).toContainText('The relayed agent reply remains readable.')

    // A separately claimed peer can send hostile protocol frames. The real
    // connector is covered above and by the HTTP duplicate-header regression.
    const session = await (await page.request.get(`${url}/api/account/session`)).json()
    const claim = await (await page.request.post(`${url}/api/installations/claim-code`, {
      headers: { 'origin': url, 'x-csrf-token': session.csrf },
      data: {},
    })).json()
    const identity = await (await page.request.post(`${url}/api/relay/claim`, {
      data: { code: claim.code, name: 'Hostile fixture', protocol: 1 },
    })).json()
    // Node's WebSocket supports request headers in its init object; DOM
    // constructor types only expose the browser's protocol argument.
    hostilePeer = Reflect.construct(WebSocket, [
      `${url.replace('http:', 'ws:')}/api/relay/${identity.installationId}/connect`,
      { headers: { authorization: `Bearer ${identity.token}` } },
    ]) as WebSocket
    const peer = hostilePeer
    const welcomed = new Promise<void>((resolve) => {
      peer.addEventListener('message', (event) => {
        const frame = JSON.parse(String(event.data))
        if (frame.type === 'welcome') {
          resolve()
          return
        }

        if (frame.type === 'request') {
          peer.send(JSON.stringify({
            type: 'response',
            id: frame.id,
            status: 200,
            headers: [
              ['content-type', 'application/json'],
              ['content-type', 'text/html'],
              ['content-security-policy', 'default-src * \'unsafe-inline\''],
            ],
            body: Buffer.from('<body>Hostile fixture<script>document.body.dataset.executed = "yes"</script>').toString('base64'),
          }))
        }
      })
    })
    await once(peer, 'open')
    peer.send(JSON.stringify({ type: 'hello', versions: [1] }))
    await welcomed

    const navigation = await page.goto(`${url}/api/installations/${identity.installationId}/api/hostile`)
    expect(navigation?.status()).toBe(200)
    // Chromium chooses the final content type; the official policy must still
    // prevent this document's script from executing on the official origin.
    expect(await page.evaluate(() => document.contentType)).toBe('text/html')
    await expect(page.locator('body')).toContainText('Hostile fixture')
    expect(await page.locator('body').getAttribute('data-executed')).toBeNull()
    expect(navigation?.headers()['content-security-policy']).toBe('sandbox')
  }
  finally {
    hostilePeer?.close()
    await Promise.all(children.map(stop))
    await seed.accounts.close()
    seed.store.close()
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})
