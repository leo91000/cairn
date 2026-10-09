import type { ChildProcess } from 'node:child_process'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
} from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('claims, detaches and reclaims the same private installation through the app', async ({ page }) => {
  test.setTimeout(90000)
  const messages: string[] = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body).text)
    response.writeHead(200).end('{}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const root = await mkdtemp(join(tmpdir(), 'leo-device-claim-'))
  await Promise.all([mkdir(join(root, 'home')), mkdir(join(root, 'data'))])
  const url = 'http://localhost:4401'
  const children: ChildProcess[] = []
  const managerEnv = {
    DATA_DIR: join(root, 'data'),
    AGENT_HOME: join(root, 'home'),
    WORKSPACE_ROOTS: root,
    NODE_ENV: 'test',
    WORKER_ENABLED: 'false',
    HOST: '127.0.0.1',
    PORT: '4402',
    LEO_OFFICIAL_ORIGIN: url,
    LEO_INSTALLATION_NAME: 'Claimed machine',
  }

  function start(binary: string, args: string[], env: NodeJS.ProcessEnv) {
    const child = spawn(binary, args, {
      env: { ...process.env, ...env, LEO_INSTALLATION_CLAIM_CODE: '' },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
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

  async function claim() {
    const child = start('target/debug/leo', ['claim'], managerEnv)
    let output = ''
    child.stdout!.on('data', chunk => output += chunk)
    await expect(async () => {
      expect(child.exitCode, 'leo claim must wait for browser approval').toBeNull()
      expect(output).toContain(`${url}/claim`)
      expect(output).toMatch(/[A-F0-9]{4}-[A-F0-9]{4}-[A-F0-9]{4}/)
    }).toPass({ timeout: 15000 })
    await page.goto(output.match(/http:\/\/localhost:4401\/claim\b/)![0])
    await expect(page.getByLabel('Device claim code')).toBeVisible()
    await page.getByLabel('Device claim code').fill(output.match(/[A-F0-9]{4}-[A-F0-9]{4}-[A-F0-9]{4}/)![0])
    await page.getByRole('button', { name: 'Review installation', exact: true }).click()
    await expect(page.getByRole('dialog', { name: 'Confirm installation claim' })).toContainText('Claimed machine')
    const fingerprint = output.match(/Installation fingerprint: ([a-f0-9]{64})/)![1]!
    await expect(page.getByLabel('Installation fingerprint')).toHaveValue(fingerprint)
    expect(child.exitCode, 'reviewing the code must not approve it').toBeNull()
    await page.getByRole('button', { name: 'Cancel claim', exact: true }).click()
    expect(child.exitCode, 'cancelling must not approve it').toBeNull()
    await page.getByRole('button', { name: 'Review installation', exact: true }).click()
    await page.getByRole('button', { name: 'Claim this installation', exact: true }).click()
    await expect(page.getByRole('status')).toContainText('Installation approved')
    await expect.poll(() => child.exitCode).toBe(0)
    await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Claimed machine · Offline', exact: true })).toBeVisible()
    const path = join(root, 'data/installation-relay/identity.json')
    expect((await stat(path)).mode & 0o777).toBe(0o600)
    return JSON.parse(await readFile(path, 'utf8'))
  }

  start('target/debug/leo-official', [], {
    LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
    LEO_OFFICIAL_ORIGIN: url,
    LEO_OFFICIAL_LISTEN: '127.0.0.1:4401',
    LEO_OFFICIAL_EMAIL_ENDPOINT: `http://127.0.0.1:${(mail.address() as { port: number }).port}/emails`,
    LEO_OFFICIAL_EMAIL_KEY: 'fixture-only',
    LEO_OFFICIAL_EMAIL_FROM: 'leo@example.test',
  })
  try {
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await page.goto(`${url}/claim`)
    await page.getByLabel('Email address').fill(`claim-${Date.now()}@example.test`)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    const cdp = await page.context().newCDPSession(page)
    await cdp.send('WebAuthn.enable')
    await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'internal',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    })
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
    await page.getByLabel('Passkey name').fill('Claim key')
    await page.getByRole('button', { name: 'Add passkey', exact: true }).click()
    await expect(page.getByText('Claim key', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Back to installations', exact: true }).click()
    // An existing (possibly offline) installation must not steal the claim URL.
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const otherCode = await page.getByLabel('Installation claim code').inputValue()
    const other = await page.request.post(`${url}/api/relay/claim`, {
      data: { code: otherCode, name: 'Other machine', protocol: 1 },
    })
    expect(other.status()).toBe(201)
    const unclaimed = start('target/debug/leo', [], managerEnv)
    await expect.poll(() => fetch('http://localhost:4402/api/chats').then(response => response.status).catch(() => 0)).toBe(401)
    await stop(unclaimed)
    const old = await claim()
    let manager = start('target/debug/leo', [], managerEnv)
    await page.getByRole('button', { name: /^Claimed machine · (Online|Offline)$/ }).click()
    await expect(async () => {
      await page.reload()
      await expect(page.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
      await expect(page.getByRole('alert')).toHaveCount(0)
    }).toPass()
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Data survives detachment')
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Data survives detachment', exact: true })).toBeVisible()
    const conversationUrl = page.url()
    // A failed replacement must leave the current private identity intact.
    const refused = start('target/debug/leo', ['claim'], managerEnv)
    await expect.poll(() => refused.exitCode).toBe(1)
    expect(JSON.stringify(JSON.parse(await readFile(join(root, 'data/installation-relay/identity.json'), 'utf8'))) === JSON.stringify(old)).toBe(true)
    await page.getByRole('button', { name: 'Installation settings', exact: true }).click()
    await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Sensitive zone', exact: true }).click()
    await page.getByRole('button', { name: 'Detach installation', exact: true }).click()
    await page.getByRole('button', { name: 'Cancel detachment', exact: true }).click()
    await page.goto(conversationUrl)
    await expect(page.getByRole('heading', { name: 'Data survives detachment', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Installation settings', exact: true }).click()
    await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Sensitive zone', exact: true }).click()
    await page.getByRole('button', { name: 'Detach installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Confirm identity before detaching this installation', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Confirm with a passkey', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Confirm detachment', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Confirm detachment', exact: true }).click()
    await expect(page).not.toHaveURL(new RegExp(old.installationId))
    const denied = await page.request.get(`${url}/api/installations/${old.installationId}/api/chats`)
    expect(denied.status()).toBe(404)
    await stop(manager)
    const renewed = await claim()
    expect(renewed.installationId).toBe(old.installationId)
    expect(renewed.token !== old.token).toBe(true)
    manager = start('target/debug/leo', [], managerEnv)
    await page.getByRole('button', { name: /^Claimed machine · (Online|Offline)$/ }).click()
    await expect(async () => {
      await page.goto(conversationUrl)
      await expect(page.getByRole('heading', { name: 'Data survives detachment', exact: true })).toBeVisible()
    }).toPass()
    await page.getByRole('button', { name: 'Installation settings', exact: true }).click()
    await page.getByRole('navigation', { name: 'Settings sections' }).getByRole('link', { name: 'Sensitive zone', exact: true }).click()
    await page.getByRole('button', { name: 'Revoke and forget installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Confirm identity before revoking this installation', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Back to settings', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Confirm revocation', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm with a passkey', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm revocation', exact: true }).click()
    await expect(page).not.toHaveURL(new RegExp(renewed.installationId))
    const revoked = await page.request.get(`${url}/api/installations/${renewed.installationId}/api/chats`)
    expect(revoked.status()).toBe(404)
  }
  finally {
    await Promise.all(children.map(stop))
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})
