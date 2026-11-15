import type { Buffer } from 'node:buffer'
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
import { setTimeout as delay } from 'node:timers/promises'
import { expect, test } from '@playwright/test'
import { relayProtocolVersion } from './relay-protocol'

// Claim journeys must leave the automatic claim variable absent, even when inherited.
function claimProcess(binary: string, args: string[], env: NodeJS.ProcessEnv) {
  const childEnv = { ...process.env, ...env }
  delete childEnv.CAIRN_INSTALLATION_CLAIM_CODE
  const child = spawn(binary, args, { env: childEnv, stdio: ['ignore', 'pipe', 'pipe'] })
  const began = Date.now()
  let output = ''
  let listener: string | undefined
  let spawnError: Error | undefined
  const retain = (chunk: Buffer) => {
    output = (output + chunk.toString()).slice(-16384)
    listener ??= output.match(/^Listening on (http:\/\/127\.0\.0\.1:\d+)\r?\n/m)?.[1]
  }

  child.stdout.on('data', retain)
  child.stderr.on('data', retain)
  child.on('error', error => spawnError = error)
  const closed = new Promise<void>(resolve => child.once('close', () => resolve()))
  const diagnostics = () => {
    let safeOutput = output.replaceAll(/(https?:\/\/|postgres(?:ql)?:\/\/)[^\s/@][^\s/:@]*:[^\s/@]+@/gi, '$1<REDACTED>@')
    for (const [key, value] of Object.entries(childEnv)) {
      if (value && value.length >= 4 && /password|secret|token|credential|api.?key/i.test(key))
        safeOutput = safeOutput.replaceAll(value, '<REDACTED>')
    }

    safeOutput = safeOutput.replaceAll(/\b[A-F0-9]{4}-[A-F0-9]{4}-[A-F0-9]{4}\b/g, '<REDACTED>')
    return {
      pid: child.pid,
      elapsedMs: Date.now() - began,
      exitCode: child.exitCode,
      signal: child.signalCode,
      listener,
      spawnError: spawnError?.message,
      output: safeOutput,
    }
  }

  return Object.assign(child, { diagnostics, closed })
}

async function stopClaimProcess(child: ReturnType<typeof claimProcess>) {
  if (child.pid === undefined || child.exitCode !== null || child.signalCode !== null) {
    await child.closed
    return
  }

  child.kill('SIGTERM')
  const force = setTimeout(() => child.kill('SIGKILL'), 10000)
  try {
    await child.closed
  }
  finally {
    clearTimeout(force)
  }
}

async function waitForUnclaimed(child: ReturnType<typeof claimProcess>) {
  const deadline = Date.now() + 7000
  let status = 0
  let requestError = ''
  while (Date.now() < deadline) {
    const state = child.diagnostics()
    if (state.exitCode !== null || state.signal !== null || state.spawnError) {
      await child.closed
      throw new Error(`Unclaimed installation exited: ${JSON.stringify(child.diagnostics())}`)
    }

    const remainingMs = deadline - Date.now()
    if (state.listener && remainingMs > 0) {
      try {
        const response = await fetch(`${state.listener}/api/chats`, { signal: AbortSignal.timeout(Math.min(1000, remainingMs)) })
        status = response.status
        await response.body?.cancel()
      }
      catch (error) {
        requestError = (error as Error).name
      }

      const after = child.diagnostics()
      if (status === 401 && Date.now() < deadline && after.exitCode === null && after.signal === null && !after.spawnError)
        return
    }

    const waitMs = Math.min(100, deadline - Date.now())
    if (waitMs > 0)
      await delay(waitMs)
  }

  throw new Error(`Unclaimed installation did not become ready within 7000ms (HTTP ${status}, ${requestError}): ${JSON.stringify(child.diagnostics())}`)
}

test('an unclaimed installation starts without contacting automatic claim, even with an inherited code', async () => {
  let claimRequests = 0
  const beacon = createServer((_request, _response) => claimRequests++)
  beacon.listen(0, '127.0.0.1')
  await once(beacon, 'listening')
  const root = await mkdtemp(join(tmpdir(), 'cairn-unclaimed-start-'))
  const child = claimProcess('target/debug/cairn', [], {
    DATA_DIR: join(root, 'data'),
    AGENT_HOME: join(root, 'home'),
    WORKSPACE_ROOTS: root,
    NODE_ENV: 'test',
    WORKER_ENABLED: 'false',
    HOST: '127.0.0.1',
    PORT: '0',
    CAIRN_BEACON_ORIGIN: `http://127.0.0.1:${(beacon.address() as { port: number }).port}`,
    CAIRN_INSTALLATION_CLAIM_CODE: 'inherited-fixture-only',
  })
  try {
    await waitForUnclaimed(child)
    expect(claimRequests).toBe(0)
  }
  finally {
    // Release a mistakenly attempted automatic claim before stopping the process.
    beacon.closeAllConnections()
    await stopClaimProcess(child)
    await new Promise<void>(resolve => beacon.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})

test('unclaimed readiness rejects another listener and reports its own installation exit', async () => {
  const occupant = createServer((_request, response) => response.writeHead(401).end())
  occupant.listen(0, '127.0.0.1')
  await once(occupant, 'listening')
  const root = await mkdtemp(join(tmpdir(), 'cairn-unclaimed-bind-'))
  const child = claimProcess('target/debug/cairn', [], {
    DATA_DIR: join(root, 'data'),
    AGENT_HOME: join(root, 'home'),
    WORKSPACE_ROOTS: root,
    NODE_ENV: 'test',
    WORKER_ENABLED: 'false',
    HOST: '127.0.0.1',
    PORT: String((occupant.address() as { port: number }).port),
  })
  try {
    await expect(waitForUnclaimed(child)).rejects.toThrow(/Unclaimed installation exited:.*Address already in use/)
    expect(child.exitCode).toBe(1)
  }
  finally {
    await stopClaimProcess(child)
    occupant.closeAllConnections()
    await new Promise<void>(resolve => occupant.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})

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
  const root = await mkdtemp(join(tmpdir(), 'cairn-device-claim-'))
  await Promise.all([mkdir(join(root, 'home')), mkdir(join(root, 'data'))])
  const url = 'http://localhost:4401'
  const children: ReturnType<typeof claimProcess>[] = []
  const managerEnv = {
    DATA_DIR: join(root, 'data'),
    AGENT_HOME: join(root, 'home'),
    WORKSPACE_ROOTS: root,
    NODE_ENV: 'test',
    WORKER_ENABLED: 'false',
    HOST: '127.0.0.1',
    PORT: '0',
    CAIRN_BEACON_ORIGIN: url,
    CAIRN_INSTALLATION_NAME: 'Claimed machine',
  }

  function start(binary: string, args: string[], env: NodeJS.ProcessEnv) {
    const child = claimProcess(binary, args, env)
    children.push(child)
    return child
  }

  async function waitForOnlineInstallation(installationId: string) {
    await expect.poll(async () => {
      const response = await page.request.get(`${url}/api/installations`)
      expect(response.ok()).toBe(true)
      const statuses = await response.json()
      const installation = statuses.find(
        (item: { id: string, online: boolean }) => item.id === installationId,
      )
      return installation?.online
    }).toBe(true)
  }

  async function claim() {
    const child = start('target/debug/cairn', ['claim'], managerEnv)
    let output = ''
    child.stdout!.on('data', chunk => output += chunk)
    await expect(async () => {
      expect(child.exitCode, 'cairn claim must wait for browser approval').toBeNull()
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

  start('target/debug/cairn-beacon', [], {
    CAIRN_BEACON_DATABASE_URL: process.env.CAIRN_BEACON_TEST_DATABASE_URL,
    CAIRN_BEACON_ORIGIN: url,
    CAIRN_BEACON_LISTEN: '127.0.0.1:4401',
    CAIRN_BEACON_EMAIL_ENDPOINT: `http://127.0.0.1:${(mail.address() as { port: number }).port}/emails`,
    CAIRN_BEACON_EMAIL_KEY: 'fixture-only',
    CAIRN_BEACON_EMAIL_FROM: 'cairn@example.test',
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
      data: { code: otherCode, name: 'Other machine', protocol: relayProtocolVersion },
    })
    expect(other.status()).toBe(201)
    const unclaimed = start('target/debug/cairn', [], managerEnv)
    await waitForUnclaimed(unclaimed)
    await stopClaimProcess(unclaimed)
    const old = await claim()
    let manager = start('target/debug/cairn', [], managerEnv)
    await page.getByRole('button', { name: /^Claimed machine · (Online|Offline)$/ }).click()
    await expect(async () => {
      await page.reload()
      await expect(page.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
      await expect(page.getByRole('alert')).toHaveCount(0)
    }).toPass()
    await waitForOnlineInstallation(old.installationId)
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Data survives detachment')
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Data survives detachment', exact: true })).toBeVisible()
    const conversationUrl = page.url()
    // A failed replacement must leave the current private identity intact.
    const refused = start('target/debug/cairn', ['claim'], managerEnv)
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
    const detached = page.waitForResponse(response => response.url() === `${url}/api/installations/${old.installationId}/detach` && response.request().method() === 'POST')
    await page.getByRole('button', { name: 'Confirm detachment', exact: true }).click()
    expect((await detached).ok()).toBe(true)
    await page.waitForURL(`${url}/`)
    const denied = await page.request.get(`${url}/api/installations/${old.installationId}/api/chats`)
    expect(denied.status()).toBe(404)
    await stopClaimProcess(manager)
    const renewed = await claim()
    expect(renewed.installationId).toBe(old.installationId)
    expect(renewed.token !== old.token).toBe(true)
    manager = start('target/debug/cairn', [], managerEnv)
    await page.getByRole('button', { name: /^Claimed machine · (Online|Offline)$/ }).click()
    await waitForOnlineInstallation(renewed.installationId)
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
    const forgotten = page.waitForResponse(response => response.url() === `${url}/api/installations/${renewed.installationId}` && response.request().method() === 'DELETE')
    await page.getByRole('button', { name: 'Confirm revocation', exact: true }).click()
    expect((await forgotten).status()).toBe(204)
    await page.waitForURL(`${url}/`)
    const revoked = await page.request.get(`${url}/api/installations/${renewed.installationId}/api/chats`)
    expect(revoked.status()).toBe(404)
  }
  finally {
    if (test.info().status !== test.info().expectedStatus) {
      await test.info().attach('claim-process-diagnostics', {
        body: JSON.stringify(children.map(child => child.diagnostics())),
        contentType: 'application/json',
      })
    }

    await Promise.all(children.map(stopClaimProcess))
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }
})
