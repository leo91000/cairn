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

test('reads and creates conversations through the previous relay protocol', async ({ page }) => {
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
  const root = await mkdtemp(join(tmpdir(), 'leo-relay-v1-'))
  await Promise.all([mkdir(join(root, 'data')), mkdir(join(root, 'home'))])
  const url = 'http://localhost:4495'
  const children: ChildProcess[] = []
  let previousPeer: WebSocket | undefined
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

  start('target/debug/leo-official', {
    LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
    LEO_OFFICIAL_ORIGIN: url,
    LEO_OFFICIAL_LISTEN: '127.0.0.1:4495',
    LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${(mail.address() as { port: number }).port}/emails`,
    LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
    LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
  })
  try {
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await page.goto(url)
    await page.getByLabel('Email address').fill(`v1-${Date.now()}@example.test`)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    start('target/debug/leo', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      WORKER_ENABLED: 'false',
      NODE_ENV: 'test',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: code,
      LEO_INSTALLATION_NAME: 'Current protocol installation',
    })
    await expect(async () => {
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass()
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByLabel('Message', { exact: true }).fill('A conversation from the current version')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    await expect(page.getByRole('heading', { name: 'A conversation from the current version', exact: true })).toBeVisible()
    const conversationUrl = page.url()
    const sourceInstallation = new URL(conversationUrl).pathname.split('/')[2]!
    const session = await (await page.request.get(`${url}/api/account/session`)).json()
    const claim = await (await page.request.post(`${url}/api/installations/claim-code`, {
      headers: { 'origin': url, 'x-csrf-token': session.csrf },
      data: {},
    })).json()
    const identity = await (await page.request.post(`${url}/api/relay/claim`, {
      data: { code: claim.code, name: 'Previous protocol installation', protocol: 1 },
    })).json()
    // Only the transport is adapted. All finite responses come from the real
    // installation through the official API, and v1 still refuses SSE.
    previousPeer = Reflect.construct(WebSocket, [
      `${url.replace('http:', 'ws:')}/api/relay/${identity.installationId}/connect`,
      { headers: { authorization: `Bearer ${identity.token}` } },
    ]) as WebSocket
    const previous = previousPeer
    const errors: unknown[] = []
    const welcomed = new Promise<void>((resolve) => {
      previous.addEventListener('message', (event) => {
        const frame = JSON.parse(String(event.data))
        if (frame.type === 'welcome') {
          expect(frame.version).toBe(1)
          resolve()
        }
        else if (frame.type === 'request') {
          void (async () => {
            const response = await page.request.fetch(`${url}/api/installations/${sourceInstallation}${frame.path}`, {
              method: frame.method,
              headers: { ...Object.fromEntries(frame.headers), 'origin': url, 'x-csrf-token': session.csrf },
              ...(frame.body ? { data: Buffer.from(frame.body, 'base64') } : {}),
            })
            const body = await response.body()
            if (previous.readyState === WebSocket.OPEN) {
              previous.send(JSON.stringify({
                type: 'response',
                id: frame.id,
                status: response.status(),
                headers: [['content-type', response.headers()['content-type'] ?? 'application/json']],
                body: body.toString('base64'),
              }))
            }
          })().catch(error => errors.push(error))
        }
      })
    })
    await once(previous, 'open')
    previous.send(JSON.stringify({ type: 'hello', versions: [1] }))
    await welcomed
    await page.goto(conversationUrl.replace(sourceInstallation, identity.installationId))
    await expect(page.getByRole('status', { name: 'Installation availability' })).toHaveText('Online')
    await expect(page.getByRole('heading', { name: 'A conversation from the current version', exact: true })).toBeVisible()
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByLabel('Message', { exact: true }).fill('A message using the previous protocol')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    await expect(page.getByRole('heading', { name: 'A message using the previous protocol', exact: true })).toBeVisible()
    expect(errors).toEqual([])
  }
  finally {
    previousPeer?.close()
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
