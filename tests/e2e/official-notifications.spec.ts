import type { Page } from '@playwright/test'
import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { createECDH, randomBytes } from 'node:crypto'
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
import { expireAccountProof } from './official-relay-fixture'

test('a browser registers once for all Leo installations and can disable account push', async ({ page, context }) => {
  test.setTimeout(120000)
  const root = await mkdtemp(join(tmpdir(), 'leo-notifications-'))
  const children: ChildProcess[] = []
  const diagnostics: string[] = []
  const messages: Array<{ to: string[], text: string }> = []
  const email = `owner-${Date.now()}@example.test`
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
  const url = 'http://localhost:4394'
  let seed: SeedService | undefined
  const registrations: string[] = []
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/api/account/notifications/subscriptions'))
      registrations.push(request.url())
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

  const curve = createECDH('prime256v1')
  curve.generateKeys()
  const subscription = {
    endpoint: 'https://fcm.googleapis.com/browser-fixture',
    keys: { p256dh: curve.getPublicKey().toString('base64url'), auth: randomBytes(16).toString('base64url') },
  }
  await context.grantPermissions(['notifications'], { origin: url })
  await page.addInitScript((data) => {
    // An existing browser subscription from the installation or an old operator key.
    if (!localStorage.getItem('fixture-push-initialized')) {
      localStorage.setItem('fixture-push-initialized', '1')
      localStorage.setItem('fixture-push', '1')
      localStorage.setItem('fixture-push-key', JSON.stringify(Array.from(new Uint8Array(65))))
    }

    Object.defineProperty(Notification, 'permission', { get: () => 'default' })
    Notification.requestPermission = async () => 'granted'
    PushManager.prototype.getSubscription = async () => localStorage.getItem('fixture-push')
      ? {
          endpoint: data.endpoint,
          expirationTime: null,
          options: { userVisibleOnly: true, applicationServerKey: Uint8Array.from(JSON.parse(localStorage.getItem('fixture-push-key')!)).buffer },
          getKey: () => null,
          toJSON: () => data,
          unsubscribe: async () => {
            localStorage.removeItem('fixture-push')
            return true
          },
        } as PushSubscription
      : null
    PushManager.prototype.subscribe = async function (options) {
      const key = options?.applicationServerKey
      if (!key || typeof key === 'string')
        throw new Error('A browser push key is required')
      const bytes = key instanceof ArrayBuffer ? new Uint8Array(key) : new Uint8Array(key.buffer, key.byteOffset, key.byteLength)
      const existingKey = localStorage.getItem('fixture-push-key')
      if (localStorage.getItem('fixture-push') && existingKey !== JSON.stringify(Array.from(bytes)))
        throw new DOMException('Unsubscribe before changing the application server key', 'InvalidStateError')

      localStorage.setItem('fixture-push-key', JSON.stringify(Array.from(bytes)))
      localStorage.setItem('fixture-push', '1')
      return (await this.getSubscription())!
    }
  }, subscription)

  const official = start('target/debug/leo-official', {
    LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
    LEO_OFFICIAL_ORIGIN: url,
    LEO_OFFICIAL_LISTEN: '127.0.0.1:4394',
    LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${(mail.address() as { port: number }).port}/emails`,
    LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
    LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
    LEO_OFFICIAL_VAPID_PRIVATE_KEY: curve.getPrivateKey().toString('base64url'),
    LEO_OFFICIAL_VAPID_SUBJECT: 'mailto:fixture@example.test',
  })
  try {
    await expect.poll(() => {
      if (official.exitCode !== null)
        throw new Error(diagnostics.join('') || `Official service exited: ${official.exitCode}`)
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }, { timeout: 30000 }).toBe(true)
    await signIn(page, email)
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Notifications', exact: true })).toBeVisible()
    expect(await (await page.request.get(`${url}/api/account/notifications`)).json()).toEqual({ publicKey: curve.getPublicKey().toString('base64url') })
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    await Promise.all(['data', 'home', 'second-data', 'second-home'].map(path => mkdir(join(root, path))))
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
    await page.getByRole('button', { name: 'Notifications', exact: true }).click()
    expireAccountProof(new URL(process.env.LEO_OFFICIAL_TEST_DATABASE_URL!), email)
    await page.getByRole('button', { name: 'Enable on this device' }).click()
    await expect(page.getByRole('alert')).toContainText('Confirm your identity')
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    const messagesBeforeProof = messages.length
    await page.getByRole('button', { name: 'Send confirmation code', exact: true }).click()
    await expect.poll(() => messages.slice(messagesBeforeProof).findLast(message => message.to.includes(email) && /\b\d{8}\b/.test(message.text))?.text).toBeTruthy()
    const proof = messages.slice(messagesBeforeProof).findLast(message => message.to.includes(email) && /\b\d{8}\b/.test(message.text))!
    await page.getByLabel('Confirmation code', { exact: true }).fill(proof.text.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Verify confirmation code', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Enable on this device' })).toBeVisible()
    await expect(page.getByText('Notifications on', { exact: true })).toHaveCount(0)
    expect(registrations).toHaveLength(1)
    await page.getByRole('button', { name: 'Enable on this device' }).click()
    await expect(page.getByText('Notifications on', { exact: true })).toBeVisible()
    expect(registrations).toHaveLength(2)
    expect(await page.evaluate(() => JSON.parse(localStorage.getItem('fixture-push-key')!))).toEqual(Array.from(curve.getPublicKey()))
    const firstInstallation = page.url()
    await page.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await page.goto(`${firstInstallation}settings`)
    await expect(page.getByRole('heading', { name: 'Question notifications', exact: true })).toHaveCount(0)
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const secondCode = await page.getByLabel('Installation claim code').inputValue()
    start('target/debug/leo', {
      DATA_DIR: join(root, 'second-data'),
      AGENT_HOME: join(root, 'second-home'),
      WORKSPACE_ROOTS: root,
      WORKER_ENABLED: 'false',
      NODE_ENV: 'test',
      PORT: '0',
      LEO_OFFICIAL_ORIGIN: url,
      LEO_INSTALLATION_CLAIM_CODE: secondCode,
      LEO_INSTALLATION_NAME: 'Second home',
    })
    await expect.poll(async () => (await (await page.request.get(`${url}/api/account/session`)).json()).installations.length, { timeout: 30000 }).toBe(2)
    await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
    await page.getByRole('combobox', { name: 'Current installation', exact: true }).selectOption({ label: 'Second home · Online' })
    await expect(page).not.toHaveURL(firstInstallation)
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Notifications', exact: true }).click()
    await expect(page.getByText('Notifications on', { exact: true })).toBeVisible()
    expect(registrations).toHaveLength(2)
    const id = (await page.evaluate(() => Object.entries(localStorage).find(([key]) => key.startsWith('leo-push-device:'))?.[1]))!
    expect(id).toMatch(/^[a-f0-9]{64}$/)
    const session = await (await page.request.get(`${url}/api/account/session`)).json()
    const path = `${url}/api/account/notifications/subscriptions/${id}`
    expect((await (await page.request.get(path)).json()).registered).toBe(true)
    await page.getByRole('button', { name: 'Disable on this device' }).click()
    await expect(page.getByRole('button', { name: 'Enable on this device' })).toBeEnabled()
    expect((await (await page.request.get(path)).json()).registered).toBe(false)
    await page.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('button', { name: 'Notifications', exact: true }).click()
    await page.getByRole('button', { name: 'Enable on this device' }).click()
    await expect(page.getByText('Notifications on', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Close dialog', exact: true }).click()
    await page.getByText('Installation options', { exact: true }).click()
    await page.getByRole('banner', { name: 'Current installation' }).getByRole('button', { name: 'Sign out', exact: true }).click()
    await expect(page.getByLabel('Email address')).toBeVisible()
    expect((await page.request.get(path)).status()).toBe(401)
    expect(session.account.id).toBeTruthy()
  }
  finally {
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
