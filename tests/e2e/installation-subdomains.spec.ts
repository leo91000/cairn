import type { AddressInfo } from 'node:net'
import type { Duplex } from 'node:stream'
import { once } from 'node:events'
import { createServer, request as httpRequest } from 'node:http'
import { connect } from 'node:net'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import { beaconRelayFixture, executeBeaconSql } from './beacon-relay-fixture'

test('installation subdomains preserve browser drafts and canonical account access', async ({ page: initialPage, browser, browserName }) => {
  let page = initialPage
  let proxyContext: Awaited<ReturnType<typeof browser.newContext>> | undefined
  test.setTimeout(120000)
  page.setDefaultTimeout(10000)
  const fixture = await beaconRelayFixture(4489)
  const database = new URL(process.env.CAIRN_BEACON_TEST_DATABASE_URL!)
  const schema = `subdomains_${crypto.randomUUID().replaceAll('-', '')}`
  executeBeaconSql(database, `CREATE SCHEMA ${schema}`)
  const isolated = new URL(database)
  isolated.searchParams.set('options', `-c search_path=${schema}`)
  let frontPort = 0
  const forwardHost = (host: string) => host.startsWith('localhost:') ? `cairn.localhost:${frontPort}` : host
  const upgraded = new Set<Duplex>()
  const proxy = createServer((request, response) => {
    const provider = new URL(request.url || '/', `http://127.0.0.1:${frontPort}`)
    if (provider.pathname === '/test/provider/authorize') {
      const callback = new URL(provider.searchParams.get('redirect_uri')!)
      callback.searchParams.set('state', provider.searchParams.get('state')!)
      callback.searchParams.set('code', 'fixture-verified')
      response.writeHead(302, { location: callback.href }).end()
      return
    }

    if (provider.pathname === '/test/provider/token') {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ access_token: 'fixture-token', token_type: 'Bearer' }))
      return
    }

    if (provider.pathname === '/test/provider/userinfo') {
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ sub: 'fixture-owner', email: 'subdomain-owner@example.test', email_verified: true }))
      return
    }

    const upstream = httpRequest({
      hostname: '127.0.0.1',
      port: 4489,
      method: request.method,
      path: request.url,
      headers: { ...request.headers, host: forwardHost(request.headers.host || '') },
    }, (source) => {
      response.writeHead(source.statusCode || 502, source.headers)
      source.pipe(response)
    })
    upstream.on('error', () => response.writeHead(502).end())
    request.pipe(upstream)
  })
  proxy.on('upgrade', (request, socket, head) => {
    upgraded.add(socket)
    socket.on('close', () => upgraded.delete(socket))
    const upstream = connect(4489, '127.0.0.1', () => {
      const headers = Object.entries({ ...request.headers, host: forwardHost(request.headers.host || '') })
        .map(([key, value]) => `${key}: ${Array.isArray(value) ? value.join(', ') : value}`)
        .join('\r\n')
      upstream.write(`${request.method} ${request.url} HTTP/${request.httpVersion}\r\n${headers}\r\n\r\n`)
      upstream.write(head)
      socket.pipe(upstream)
      upstream.pipe(socket)
    })
    upstream.on('error', () => socket.destroy())
    socket.on('error', () => upstream.destroy())
  })
  proxy.listen(0, '127.0.0.1')
  await once(proxy, 'listening')
  frontPort = (proxy.address() as AddressInfo).port
  const origin = `http://cairn.localhost:${frontPort}`
  try {
    if (browserName === 'webkit') {
      proxyContext = await browser.newContext({ proxy: { server: `http://127.0.0.1:${frontPort}` } })
      page = await proxyContext.newPage()
      page.setDefaultTimeout(10000)
    }

    fixture.beacon(isolated.toString(), {
      CAIRN_BEACON_ORIGIN: origin,
      CAIRN_BEACON_INSTALLATION_SUBDOMAINS: 'true',
      CAIRN_BEACON_GOOGLE_CLIENT_ID: 'fixture-google',
      CAIRN_BEACON_GOOGLE_CLIENT_SECRET: 'fixture-only',
      CAIRN_BEACON_GOOGLE_AUTHORIZATION_URL: `http://127.0.0.1:${frontPort}/test/provider/authorize`,
      CAIRN_BEACON_GOOGLE_TOKEN_URL: `http://127.0.0.1:${frontPort}/test/provider/token`,
      CAIRN_BEACON_GOOGLE_USERINFO_URL: `http://127.0.0.1:${frontPort}/test/provider/userinfo`,
    })
    await expect.poll(() => fetch(`${fixture.url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
    await page.goto(origin)
    await page.getByLabel('Email address').fill('subdomain-owner@example.test')
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => fixture.messages.length).toBe(1)
    await page.getByLabel('Email code').fill(fixture.messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const code = await page.getByLabel('Installation claim code').inputValue()
    fixture.start('target/debug/cairn', {
      DATA_DIR: join(fixture.root, 'data'),
      AGENT_HOME: join(fixture.root, 'home'),
      WORKSPACE_ROOTS: fixture.root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      PORT: '0',
      CAIRN_BEACON_ORIGIN: `http://localhost:${frontPort}`,
      CAIRN_INSTALLATION_CLAIM_CODE: code,
      CAIRN_INSTALLATION_NAME: 'Subdomain installation',
    })
    await expect(async () => {
      await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
      await expect(page.getByRole('navigation', { name: 'Workspace navigation', exact: true })).toBeVisible()
    }).toPass({ timeout: 20000 })
    const hostedOrigin = new URL(page.url()).origin
    expect(hostedOrigin).toMatch(/http:\/\/[a-z]+-[a-z]+-[a-f0-9]{8}\.cairn\.localhost:/)
    await page.goto(`${hostedOrigin}/beacon.html`)
    await expect(page).toHaveURL(`${hostedOrigin}/`)
    const session = await page.evaluate(async () => (await fetch('/api/account/session')).json())
    const installation = session.installations[0]
    const csrf = { 'origin': hostedOrigin, 'x-csrf-token': session.csrf }
    const chat = await page.evaluate(async ({ id, headers }) => (await fetch(`/api/installations/${id}/api/chats`, { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: '{}' })).json(), { id: installation.id, headers: csrf })
    const legacy = `${origin}/installations/${installation.id}/chats/${chat.id}?bookmark=yes#reply`
    await page.goto(origin)
    await expect(page).toHaveURL(`${hostedOrigin}/`)
    await page.goto(`${origin}/installations/${installation.slug}/settings/account`)
    await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()
    await page.evaluate(({ id, chatId }) => sessionStorage.setItem(`cairn-chat-draft:${id}:${chatId}`, 'Keep this draft across origins'), { id: installation.id, chatId: chat.id })
    await page.goto(legacy)
    await expect(page).toHaveURL(`${hostedOrigin}/chats/${chat.id}?bookmark=yes#reply`)
    await expect(page.getByLabel('Message', { exact: true })).toHaveValue('Keep this draft across origins')
    await page.reload()
    await expect(page.getByLabel('Message', { exact: true })).toHaveValue('Keep this draft across origins')
    await page.getByRole('button', { name: /^(Send|Queue)$/, exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Keep this draft across origins', exact: true })).toBeVisible()
    await page.goto(legacy)
    await expect(page).toHaveURL(`${hostedOrigin}/chats/${chat.id}?bookmark=yes#reply`)
    await expect(page.getByLabel('Message', { exact: true })).toHaveValue('')

    const cookie = (await page.context().cookies(origin)).find(cookie => cookie.name === 'cairn_session')!
    expect(cookie.domain).toBe('.cairn.localhost')
    expect(cookie.httpOnly).toBe(true)

    // Private assets and a live stream keep using the installation's internal UUID.
    const attachmentId = crypto.randomUUID()
    const attachmentPath = `/api/installations/${installation.id}/api/chats/${chat.slug}/attachments/${attachmentId}`
    const attachment = await page.evaluate(async ({ path, headers }) => {
      const uploaded = await fetch(`${path}?name=notes.txt`, {
        method: 'PUT',
        headers: { ...headers, 'Content-Type': 'application/octet-stream' },
        body: 'A private attachment',
      })
      return { uploaded: uploaded.ok, downloaded: await (await fetch(path)).text() }
    }, { path: attachmentPath, headers: csrf })
    expect(attachment.uploaded).toBe(true)
    expect(attachment.downloaded).toBe('A private attachment')
    const pendingMailboxes = await page.evaluate(() => Object.keys(localStorage).filter(key => key.startsWith('cairn-draft-transfer:')))
    expect(pendingMailboxes).toEqual([])
    if (browserName === 'webkit') {
      await page.getByRole('navigation', { name: 'Workspace navigation', exact: true }).getByRole('button', { name: 'Sign out', exact: true }).click()
      await page.goto(`${hostedOrigin}/beacon.html`)
      await expect(page.getByLabel('Email address')).toBeVisible()
      await expect.poll(() => new URL(page.url()).origin).toBe(origin)
      return
    }

    await page.goto(`${hostedOrigin}/settings`)
    await expect(page.getByRole('button', { name: 'Add passkey', exact: true })).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Send confirmation code', exact: true })).toHaveCount(0)
    await page.getByRole('button', { name: 'Open account settings', exact: true }).click()
    await expect(page).toHaveURL(new RegExp(`^${origin}/installations/${installation.slug}/settings/account\\?return_to=`))
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
    await page.getByRole('button', { name: 'Add passkey', exact: true }).click()
    await expect(page.getByText('My passkey', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Add Google', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Remove Google subdomain-owner@example.test', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Back to installation', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/`)
    await page.goto(`${hostedOrigin}/chats/${chat.slug}`)
    await expect(page.getByRole('heading', { name: 'Keep this draft across origins', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Chat details', exact: true }).click()
    await page.getByRole('link', { name: 'Question notifications', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Profile', exact: true })).toBeVisible()
    expect(new URL(page.url()).origin).toBe(origin)
    executeBeaconSql(isolated, 'UPDATE web_sessions SET last_proof_at=clock_timestamp() - interval \'1 hour\'')
    await page.goto(`${hostedOrigin}/settings/sensitive`)
    await page.getByRole('button', { name: 'Revoke and forget installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await expect(page.getByRole('dialog', { name: 'Confirm identity', exact: true })).toBeVisible()
    expect(new URL(page.url()).origin).toBe(origin)
    await page.getByRole('button', { name: 'Confirm with a passkey', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/settings/sensitive`)
    await page.goto(`${hostedOrigin}/chats/${chat.slug}`)
    await page.getByRole('navigation', { name: 'Workspace navigation', exact: true }).getByRole('button', { name: 'Sign out', exact: true }).click()
    await expect(page).toHaveURL(`${origin}/`)
    await page.goto(`${hostedOrigin}/beacon.html`)
    await expect(page.getByLabel('Email address')).toBeVisible()
    expect(new URL(page.url()).origin).toBe(origin)
    await page.goto(`${hostedOrigin}/chats/${chat.slug}`)
    await expect(page.getByLabel('Email address')).toBeVisible()
    expect(new URL(page.url()).origin).toBe(origin)
    await page.getByRole('button', { name: 'Sign in with a passkey', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/chats/${chat.slug}`)
    await expect(page.getByRole('heading', { name: 'Keep this draft across origins', exact: true })).toBeVisible()
    await page.getByRole('navigation', { name: 'Workspace navigation', exact: true }).getByRole('button', { name: 'Sign out', exact: true }).click()
    await expect(page).toHaveURL(`${origin}/`)
    const oauthDeepLink = `${hostedOrigin}/chats/${chat.slug}?source=oauth#reply`
    await page.goto(oauthDeepLink)
    await expect(page.getByLabel('Email address')).toBeVisible()
    await page.getByRole('button', { name: 'Continue with Google', exact: true }).click()
    await expect(page).toHaveURL(oauthDeepLink)
    await expect(page.getByLabel('Message', { exact: true })).toHaveValue('')

    // Establish a fresh proof on the canonical origin before changing access.
    await page.goto(`${hostedOrigin}/settings/sensitive`)
    await page.getByRole('button', { name: 'Revoke and forget installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm with a passkey', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/settings/sensitive`)
    await page.goto(`${origin}/installations/${installation.slug}/settings/account`)
    const extra = await page.evaluate(async () => {
      const session = await (await fetch('/api/account/session')).json()
      const headers = { 'X-CSRF-Token': session.csrf, 'Content-Type': 'application/json' }
      const claimed: string[] = []
      for (const name of ['Detached installation', 'Shared installation']) {
        const code = await (await fetch('/api/installations/claim-code', { method: 'POST', headers, body: '{}' })).json()
        const identity = await (await fetch('/api/relay/claim', { method: 'POST', headers, body: JSON.stringify({ code: code.code, name, protocol: 4 }) })).json()
        claimed.push(identity.installationId)
      }

      const listed = await (await fetch('/api/account/session')).json()
      return listed.installations.filter((item: { id: string }) => claimed.includes(item.id))
    })
    const detached = extra.find((item: { name: string }) => item.name === 'Detached installation')
    const shared = extra.find((item: { name: string }) => item.name === 'Shared installation')
    const secondOwner = crypto.randomUUID()
    executeBeaconSql(isolated, `INSERT INTO cairn_accounts (id,email) VALUES ('${secondOwner}','second-owner@example.test'); UPDATE installations SET owner_id='${secondOwner}' WHERE id='${shared.id}'; INSERT INTO installation_members (installation_id,account_id) VALUES ('${shared.id}','${session.account.id}')`)
    const detachedOrigin = origin.replace('cairn.localhost', `${detached.slug}.cairn.localhost`)
    const sharedOrigin = origin.replace('cairn.localhost', `${shared.slug}.cairn.localhost`)
    await page.goto(`${sharedOrigin}/settings/sensitive`)
    await page.getByRole('button', { name: 'Leave installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm leaving', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/`)
    await page.goto(`${detachedOrigin}/settings/sensitive`)
    await page.getByRole('button', { name: 'Detach installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm detachment', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/`)
    await page.goto(`${hostedOrigin}/settings/sensitive`)
    await page.getByRole('button', { name: 'Revoke and forget installation', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm revocation', exact: true }).click()
    await expect(page).toHaveURL(`${origin}/`)
    await expect(page.getByRole('heading', { name: 'No installations yet', exact: true })).toBeVisible()
    const afterAccessChanges = await page.evaluate(async () => (await fetch('/api/account/session')).json())
    expect(afterAccessChanges.installations).toEqual([])
  }
  finally {
    await proxyContext?.close()
    await fixture.close()
    for (const socket of upgraded)
      socket.destroy()
    proxy.closeAllConnections()
    await new Promise<void>(resolve => proxy.close(() => resolve()))
    executeBeaconSql(database, `DROP SCHEMA ${schema} CASCADE`)
  }
})
