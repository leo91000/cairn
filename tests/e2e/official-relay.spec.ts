import { Buffer } from 'node:buffer'
import { once } from 'node:events'
import { join } from 'node:path'
import { expect, test } from '@playwright/test'
import { officialRelayFixture } from './official-relay-fixture'

test('claims an installation and sends after relay restarts and official session renewal', async ({ page }) => {
  test.setTimeout(120000)
  const fixture = await officialRelayFixture()
  const {
    root,
    url,
    messages,
    seed,
    start,
    stop,
    official,
  } = fixture
  let hostilePeer: WebSocket | undefined
  let service = official()
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
    const installation = start('target/debug/cairn', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      // This journey deliberately exercises HTTP relay restart/history contracts.
      CAIRN_DIRECT_ENABLED: 'false',
      PORT: '0',
      CAIRN_BEACON_ORIGIN: url,
      CAIRN_INSTALLATION_CLAIM_CODE: code,
      CAIRN_INSTALLATION_NAME: 'Browser installation',
    })
    await expect(async () => {
      expect(installation.exitCode, 'installation must remain running').toBeNull()
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
    }).toPass()
    await page.getByRole('link', { name: 'New conversation', exact: true }).first().click()
    await page.getByLabel('Message', { exact: true }).fill('A message through the relay')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    await expect(page.getByRole('heading', { name: 'A message through the relay', exact: true })).toBeVisible()
    const conversationUrl = page.url()
    const availability = page.getByRole('status', { name: 'Installation availability', exact: true })
    await expect(availability).toHaveText('Online')
    await page.reload()
    await expect(page.getByRole('heading', { name: 'A message through the relay', exact: true })).toBeVisible()
    // A process restart really closes every relay socket, unlike stopping a listener.
    const resumedStream = page.waitForResponse(response => response.url().includes('/api/installations/') && /\/stream\?/.test(response.url()) && response.status() === 200, { timeout: 20000 })
    await stop(service)
    service = official()
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await resumedStream
    await expect(page.getByRole('alert')).toHaveCount(0)
    await expect(page.getByRole('heading', { name: 'A message through the relay', exact: true })).toBeVisible()
    await stop(installation)
    await expect(availability).toHaveText('Offline', { timeout: 15000 })
    start('target/debug/cairn', {
      DATA_DIR: join(root, 'data'),
      AGENT_HOME: join(root, 'home'),
      WORKSPACE_ROOTS: root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      // This journey deliberately exercises HTTP relay restart/history contracts.
      CAIRN_DIRECT_ENABLED: 'false',
      PORT: '0',
    })
    await expect(availability).toHaveText('Online', { timeout: 15000 })
    await expect(page.getByRole('alert')).toHaveCount(0)
    await expect(availability).toHaveText('Online', { timeout: 15000 })
    await page.getByLabel('Message', { exact: true }).fill('After reconnection')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    await expect(page.getByText('After reconnection', { exact: true })).toBeVisible()
    // Renew the official session while keeping the same installation selected.
    // Use the existing account UI and a resident authenticator, as in #52.
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
    await page.getByRole('button', { name: 'Account', exact: true }).click()
    await page.getByRole('menuitem', { name: 'Account settings', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Sign-in methods', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Add passkey', exact: true }).click()
    await expect(page.getByText('My passkey', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: /Remove Email/ }).click()
    // The previous login's email delivery quota expires after one minute. Keep
    // that real limit enabled and retry through the public UI until it expires.
    await expect(async () => {
      await page.getByRole('button', { name: 'Enable email sign-in', exact: true }).click()
      await expect(page.getByLabel('Email code')).toBeVisible()
    }).toPass({ timeout: 70000 })
    await expect.poll(() => messages.length).toBe(2)
    await page.getByLabel('Email code').fill(messages[1]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Confirm email code', exact: true }).click()
    await expect(page.getByRole('button', { name: /Remove Email/ })).toBeVisible()
    await page.goto(conversationUrl)
    await page.getByLabel('Message', { exact: true }).fill('After session renewal')
    const sent = page.waitForResponse(response => response.url().endsWith('/messages') && response.request().method() === 'POST')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    expect((await sent).status()).toBe(200)
    await page.getByRole('button', { name: '+ 1 other message', exact: true }).click()
    await expect(page.getByText('After session renewal', { exact: true })).toBeVisible()

    // Seed a finished run using the same fixture module as the native browser
    // journeys. Its detail is then read through the real official HTTP relay.
    const chat = seed.store.list('chats')[0]!
    const task = seed.task({ name: 'Finished conversation', prompt: 'Review the workspace', agentId: chat.agentId })
    const run = await seed.enqueue(task.id)
    seed.store.updateRun(run.id, { status: 'succeeded', summary: 'The **relayed agent reply** remains readable.', finishedAt: Date.now() })
    seed.store.event(run.id, 'item.completed', 'The **relayed agent reply** remains readable.', { item: { id: 'relayed-reply', type: 'agent_message', text: 'The **relayed agent reply** remains readable.' } })
    seed.store.put('chats', { ...chat, runId: run.id })
    // The first SSE batch can render before the initial finite compatibility
    // read finishes. Observe only subsequent delivery when checking for polling.
    const initialHistory = page.waitForResponse(response => response.url().includes(`/runs/${run.id}/events?after=0&limit=500`) && response.status() === 200)
    await page.reload()
    await expect(page.locator('.activity-message').filter({ hasText: 'The relayed agent reply remains readable.' })).toBeVisible()
    await (await initialHistory).finished()
    // An external installation commit reaches the open browser stream without
    // navigation or the old three-second snapshot polling.
    const liveRequests: string[] = []
    const collectReads = (request: import('@playwright/test').Request) => {
      if (request.method() === 'GET' && /\/api\/installations\/.*\/(?:events|artifacts)(?:\?|$)/.test(request.url()))
        liveRequests.push(request.url())
    }

    page.on('request', collectReads)
    seed.store.event(run.id, 'item.completed', 'A live update through the relay', {
      item: { id: 'relayed-live-update', type: 'agent_message', text: 'A live update through the relay' },
    })
    await expect(page.locator('.activity-message').filter({ hasText: 'A live update through the relay' })).toBeVisible({ timeout: 20000 })
    expect(liveRequests).toEqual([])
    page.off('request', collectReads)

    await page.getByRole('navigation', { name: 'Workspace navigation', exact: true }).getByRole('button', { name: 'Sign out', exact: true }).click()
    await expect(page).toHaveURL(`${url}/`)
    await expect(page.getByLabel('Email address')).toBeVisible()
    await page.goto(conversationUrl)
    await page.getByRole('button', { name: 'Sign in with a passkey', exact: true }).click()
    await expect(page).toHaveURL(conversationUrl)
    await expect(page.locator('.activity-message').filter({ hasText: 'The relayed agent reply remains readable.' })).toBeVisible()

    const session = await (await page.request.get(`${url}/api/account/session`)).json()
    // A separately claimed peer can send hostile protocol frames. The real
    // connector is covered above and by the HTTP duplicate-header regression.
    const claim = await (await page.request.post(`${url}/api/installations/claim-code`, {
      headers: { 'origin': url, 'x-csrf-token': session.csrf },
      data: {},
    })).json()
    const identity = await (await page.request.post(`${url}/api/relay/claim`, {
      data: { code: claim.code, name: 'Hostile fixture', protocol: 1 },
    })).json()
    const incompatible = Reflect.construct(WebSocket, [
      `${url.replace('http:', 'ws:')}/api/relay/${identity.installationId}/connect`,
      { headers: { authorization: `Bearer ${identity.token}` } },
    ]) as WebSocket
    await once(incompatible, 'open')
    const refused = once(incompatible, 'close')
    incompatible.send(JSON.stringify({ type: 'hello', versions: [99] }))
    await refused
    await stop(service)
    service = official()
    await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await page.goto(`${url}/installations/${identity.installationId}/`)
    await expect(page.getByRole('status', { name: 'Installation availability' })).toHaveText('Mise à jour nécessaire')
    await expect(page.getByRole('alert')).toContainText('Mise à jour nécessaire')
    await expect(page.getByRole('navigation', { name: 'Workspace navigation', exact: true })).toHaveCount(0)
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 900 })
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    }

    await page.setViewportSize({ width: 1440, height: 900 })

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
    await expect(page.getByRole('status', { name: 'Installation availability' })).toHaveText('Online')

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
    await fixture.close()
  }
})
