import type { AddressInfo } from 'node:net'
import type { Duplex } from 'node:stream'
import { once } from 'node:events'
import { createServer, request as httpRequest } from 'node:http'
import { connect } from 'node:net'
import { join } from 'node:path'
import process from 'node:process'
import { expect, test } from '@playwright/test'
import { beaconRelayFixture, executeBeaconSql } from './beacon-relay-fixture'

test('installation subdomains keep central login, drafts, account confirmation and revocation', async ({ page }) => {
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
    fixture.beacon(isolated.toString(), {
      CAIRN_BEACON_ORIGIN: origin,
      CAIRN_BEACON_INSTALLATION_SUBDOMAINS: 'true',
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

    await page.getByRole('button', { name: 'Account', exact: true }).click()
    await page.getByRole('menuitem', { name: 'Account settings', exact: true }).click()
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
    await page.getByRole('button', { name: 'Back to installation', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/`)
    await page.goto(`${hostedOrigin}/chats/${chat.slug}`)
    await expect(page.getByRole('heading', { name: 'Keep this draft across origins', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Question notifications', exact: true }).click()
    await page.getByRole('button', { name: 'Manage notifications', exact: true }).click()
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
    await page.goto(`${hostedOrigin}/chats/${chat.slug}`)
    await expect(page.getByLabel('Email address')).toBeVisible()
    expect(new URL(page.url()).origin).toBe(origin)
    await page.getByRole('button', { name: 'Sign in with a passkey', exact: true }).click()
    await expect(page).toHaveURL(`${hostedOrigin}/chats/${chat.slug}`)
    await expect(page.getByRole('heading', { name: 'Keep this draft across origins', exact: true })).toBeVisible()
  }
  finally {
    await fixture.close()
    for (const socket of upgraded)
      socket.destroy()
    proxy.closeAllConnections()
    await new Promise<void>(resolve => proxy.close(() => resolve()))
    executeBeaconSql(database, `DROP SCHEMA ${schema} CASCADE`)
  }
})
