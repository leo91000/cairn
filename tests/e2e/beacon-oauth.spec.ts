import { Buffer } from 'node:buffer'
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('Google and GitHub reuse an account, manage sign-in methods and preserve claim navigation', async ({ page, request }) => {
  test.setTimeout(90000)
  const messages: string[] = []
  let denyNextAuthorization = false
  let failNextToken = false
  const githubId = Date.now()
  const email = `oauth-browser-${Date.now()}@example.test`
  const provider = createServer(async (request, response) => {
    const url = new URL(request.url!, 'http://localhost')
    response.setHeader('content-type', 'application/json')
    if (url.pathname === '/authorize') {
      const callback = new URL(url.searchParams.get('redirect_uri')!)
      callback.searchParams.set('state', url.searchParams.get('state')!)
      if (denyNextAuthorization) {
        callback.searchParams.set('error', 'access_denied')
        denyNextAuthorization = false
      }
      else {
        callback.searchParams.set('code', 'verified')
      }

      expect(url.searchParams.get('scope')).toBe(url.searchParams.get('client_id') === 'google-test' ? 'openid email' : 'user:email')
      expect(url.searchParams.get('code_challenge_method')).toBe('S256')
      response.writeHead(302, { location: callback.href }).end()
    }
    else if (url.pathname === '/token') {
      let text = ''
      for await (const chunk of request)
        text += chunk
      const body = new URLSearchParams(text)
      expect(body.get('client_secret')).toBe('test-only')
      expect(body.get('code_verifier')!.length).toBeGreaterThanOrEqual(43)
      if (failNextToken) {
        failNextToken = false
        response.writeHead(503).end(JSON.stringify({ error: 'private-provider-body' }))
        return
      }

      response.end(JSON.stringify({ access_token: 'test-token', token_type: 'Bearer' }))
    }
    else if (url.pathname === '/github/applications/github-test/token' && request.method === 'DELETE') {
      expect(request.headers.authorization).toBe(`Basic ${Buffer.from('github-test:test-only').toString('base64')}`)
      let text = ''
      for await (const chunk of request)
        text += chunk
      expect(JSON.parse(text)).toEqual({ access_token: 'test-token' })
      response.writeHead(204).end()
    }
    else if (url.pathname === '/google/userinfo') {
      response.end(JSON.stringify({ sub: email, email, email_verified: true }))
    }
    else if (url.pathname === '/github/user') {
      response.end(JSON.stringify({ id: githubId, email: 'untrusted@example.test' }))
    }
    else if (url.pathname === '/github/emails') {
      response.end(JSON.stringify([{ email, verified: true, primary: true }]))
    }
    else if (url.pathname === '/email') {
      let text = ''
      for await (const chunk of request)
        text += chunk
      messages.push(JSON.parse(text).text)
      response.end('{}')
    }
    else if (url.pathname === '/mcp-test-callback') {
      response.setHeader('content-type', 'text/html')
      response.end('<!doctype html><title>MCP client</title><p>Authorization received</p>')
    }
    else {
      response.writeHead(404).end('{}')
    }
  })
  provider.listen(0, '127.0.0.1')
  await once(provider, 'listening')
  const providerUrl = `http://127.0.0.1:${(provider.address() as { port: number }).port}`
  const url = 'http://localhost:4399'
  const child = spawn('target/debug/cairn-beacon', [], {
    env: {
      ...process.env,
      CAIRN_BEACON_DATABASE_URL: process.env.CAIRN_BEACON_TEST_DATABASE_URL,
      CAIRN_BEACON_ORIGIN: url,
      CAIRN_BEACON_LISTEN: '127.0.0.1:4399',
      CAIRN_BEACON_EMAIL_ENDPOINT: `${providerUrl}/email`,
      CAIRN_BEACON_EMAIL_KEY: 'test-only',
      CAIRN_BEACON_EMAIL_FROM: 'Cairn <cairn@example.test>',
      CAIRN_BEACON_GOOGLE_CLIENT_ID: 'google-test',
      CAIRN_BEACON_GOOGLE_CLIENT_SECRET: 'test-only',
      CAIRN_BEACON_GOOGLE_AUTHORIZATION_URL: `${providerUrl}/authorize`,
      CAIRN_BEACON_GOOGLE_TOKEN_URL: `${providerUrl}/token`,
      CAIRN_BEACON_GOOGLE_USERINFO_URL: `${providerUrl}/google/userinfo`,
      CAIRN_BEACON_GITHUB_CLIENT_ID: 'github-test',
      CAIRN_BEACON_GITHUB_CLIENT_SECRET: 'test-only',
      CAIRN_BEACON_GITHUB_AUTHORIZATION_URL: `${providerUrl}/authorize`,
      CAIRN_BEACON_GITHUB_TOKEN_URL: `${providerUrl}/token`,
      CAIRN_BEACON_GITHUB_USERINFO_URL: `${providerUrl}/github/user`,
      CAIRN_BEACON_GITHUB_EMAILS_URL: `${providerUrl}/github/emails`,
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  const exited = once(child, 'exit')
  let log = ''
  child.stderr.on('data', chunk => log += chunk)
  try {
    await expect.poll(async () => {
      if (child.exitCode !== null)
        throw new Error(`Beacon exited: ${log}`)
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }, { timeout: 30000 }).toBe(true)
    await page.goto(url)
    await expect(page.getByRole('button', { name: 'Continue with Google' })).toBeVisible()
    denyNextAuthorization = true
    await page.getByRole('button', { name: 'Continue with Google' }).click()
    await expect(page.getByRole('alert')).toContainText('Sign-in was cancelled')
    expect((await page.context().cookies(url)).some(cookie => cookie.name === 'cairn_oauth')).toBe(false)
    failNextToken = true
    await page.getByRole('button', { name: 'Continue with Google' }).click()
    await expect(page.getByRole('heading', { name: 'Sign-in provider unavailable' })).toBeVisible()
    await expect(page.locator('body')).not.toContainText('private-provider-body')
    await page.getByRole('link', { name: 'Try again', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Sign-in provider unavailable')
    await page.getByRole('button', { name: 'Continue with Google' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await expect(page.getByText(`You’re signed in as ${email}.`)).toBeVisible()
    const first = await page.evaluate(() => fetch('/api/account/session').then(response => response.json()))
    const unconfirmed = await page.request.post(`${url}/api/account/delete`, {
      headers: { 'origin': url, 'x-csrf-token': first.csrf },
      data: { email },
    })
    expect(unconfirmed.status()).toBe(403)
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
    await page.getByRole('button', { name: 'Add GitHub', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Confirm your identity')
    await expect(page.getByRole('heading', { name: 'Sign-in methods', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Add passkey', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Confirm your identity')
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Confirm identity before changing sign-in methods', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Confirm account deletion', exact: true })).toHaveCount(0)
    await page.getByRole('button', { name: 'Send confirmation code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Confirmation code', { exact: true }).fill('wrong')
    await page.getByRole('button', { name: 'Verify confirmation code', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Invalid or expired code')
    await expect(page.getByRole('heading', { name: 'Confirm identity', exact: true })).toBeVisible()
    await page.getByLabel('Confirmation code', { exact: true }).fill(messages[0]!.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Verify confirmation code', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Sign-in methods', exact: true })).toBeVisible()
    await expect(page.getByRole('button', { name: `Remove Google ${email}` })).toBeDisabled()
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
    await page.getByLabel('Passkey name').fill('Account key')
    await page.getByRole('button', { name: 'Add passkey', exact: true }).click()
    await expect(page.getByText('Account key', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Add GitHub' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    const linked = await page.evaluate(() => fetch('/api/account/session').then(response => response.json()))
    expect(linked.account).toEqual(first.account)
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
    await page.getByRole('button', { name: `Remove Google ${email}` }).click()
    await expect(page.getByRole('alert')).toContainText('Confirm your identity')
    await expect(page.getByRole('button', { name: `Remove Google ${email}` })).toBeVisible()
    await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
    await page.getByRole('button', { name: 'Confirm with a passkey', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Sign-in methods', exact: true })).toBeVisible()
    await page.getByRole('button', { name: `Remove Google ${email}` }).click()

    await expect(page.getByRole('button', { name: 'Add Google' })).toBeVisible()
    await page.getByRole('button', { name: 'Add Google' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page.getByLabel('Email address')).toBeVisible()
    const deepUrl = `${url}/installations/00000000-0000-0000-0000-000000000049/agents`
    await page.goto(deepUrl)
    denyNextAuthorization = true
    await page.getByRole('button', { name: 'Continue with GitHub' }).click()
    await expect(page.getByRole('alert')).toContainText('Sign-in was cancelled')
    await expect(page).toHaveURL(deepUrl)
    await page.getByRole('button', { name: 'Continue with GitHub' }).click()
    await expect(page.getByRole('alert')).toContainText('unavailable or no longer accessible')
    await expect(page).toHaveURL(deepUrl)
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    const signedIn = await page.evaluate(() => fetch('/api/account/session').then(response => response.json()))
    expect(signedIn.account).toEqual(first.account)

    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const otherCode = await page.getByLabel('Installation claim code').inputValue()
    const other = await page.request.post(`${url}/api/relay/claim`, {
      data: { code: otherCode, name: 'Existing OAuth machine', protocol: 1 },
    })
    expect(other.status()).toBe(201)

    const started = await page.request.post(`${url}/api/relay/device-claim/start`, {
      data: { name: 'OAuth claim machine', protocol: 1 },
    })
    expect(started.status()).toBe(201)
    const device = await started.json()
    await page.getByRole('button', { name: 'Sign out', exact: true }).click()
    await expect(page.getByLabel('Email address')).toBeVisible()
    await expect(page).toHaveURL(`${url}/`)
    await page.goto(device.verificationUri)
    await page.getByRole('button', { name: 'Continue with GitHub' }).click()
    await expect(page.getByLabel('Device claim code')).toBeVisible()
    await expect(page).toHaveURL(device.verificationUri)
    await page.getByLabel('Device claim code').fill(device.userCode)
    await page.getByRole('button', { name: 'Review installation', exact: true }).click()
    await expect(page.getByRole('dialog', { name: 'Confirm installation claim' })).toContainText('OAuth claim machine')
    await expect(page.getByLabel('Installation fingerprint')).toHaveValue(device.fingerprint)
    await page.getByRole('button', { name: 'Claim this installation', exact: true }).click()
    await expect(page.getByRole('status')).toContainText('Installation approved')
    const claimed = await page.request.post(`${url}/api/relay/device-claim/poll`, {
      data: { deviceCode: device.deviceCode },
    })
    expect(claimed.status()).toBe(200)
    expect((await claimed.json()).installationId).toBeTruthy()
    const callbackUrl = `${providerUrl}/mcp-test-callback`
    const clientResponse = await page.request.post(`${url}/oauth/register`, {
      data: { client_name: 'Sign-in MCP client', redirect_uris: [callbackUrl] },
    })
    expect(clientResponse.status()).toBe(201)
    const client = await clientResponse.json()
    const parameters = new URLSearchParams({
      client_id: client.client_id,
      redirect_uri: callbackUrl,
      response_type: 'code',
      code_challenge_method: 'S256',
      code_challenge: createHash('sha256').update('a'.repeat(43)).digest('base64url'),
      state: 'resume-mcp-consent',
      scope: 'read',
    })
    for (const provider of ['Google', 'GitHub']) {
      const current = await (await page.request.get(`${url}/api/account/session`)).json()
      expect((await page.request.post(`${url}/api/account/logout`, {
        headers: { 'origin': url, 'x-csrf-token': current.csrf },
        data: {},
      })).status()).toBe(204)
      await page.goto(`${url}/oauth/authorize?${parameters}`)
      await page.getByRole('button', { name: `Continue with ${provider}`, exact: true }).click()
      await expect(page.getByRole('heading', { name: 'Connect an assistant', exact: true })).toBeVisible()
      await expect(page).toHaveURL(/\/authorize\?/)
      expect(Object.fromEntries(new URL(page.url()).searchParams)).toEqual(Object.fromEntries(parameters))
      await page.getByRole('combobox', { name: 'Installation', exact: true }).click()
      await page.getByRole('option', { name: 'OAuth claim machine', exact: true }).click()
      await page.getByRole('button', { name: 'Allow access', exact: true }).click()
      await expect(page.getByRole('alert').filter({ hasText: 'Confirm your identity' })).toBeVisible()
      await page.getByRole('button', { name: 'Confirm identity', exact: true }).click()
      await page.getByRole('button', { name: 'Confirm with a passkey', exact: true }).click()
      await expect(page.getByRole('heading', { name: 'Connect an assistant', exact: true })).toBeVisible()
      expect(Object.fromEntries(new URL(page.url()).searchParams)).toEqual(Object.fromEntries(parameters))
      await expect(page.getByRole('combobox', { name: 'Installation', exact: true })).toHaveValue('OAuth claim machine')
      await page.getByRole('button', { name: 'Allow access', exact: true }).click()
      await expect(page).toHaveURL(/mcp-test-callback\?state=resume-mcp-consent&code=/)
      expect(new URL(page.url()).origin).toBe(providerUrl)
    }

    const unverified = await page.request.post(`${url}/oauth/register`, {
      data: { client_name: 'Claude Desktop', redirect_uris: ['https://evil.example/cb'] },
    })
    expect(unverified.status()).toBe(201)
    const maliciousClient = await unverified.json()
    const unverifiedParameters = new URLSearchParams(parameters)
    unverifiedParameters.set('client_id', maliciousClient.client_id)
    unverifiedParameters.set('redirect_uri', 'https://evil.example/cb')
    unverifiedParameters.set('scope', 'read run manage')
    await page.goto(`${url}/oauth/authorize?${unverifiedParameters}`)
    await expect(page.getByRole('heading', { name: 'Connect an assistant', exact: true })).toBeVisible()
    await expect(page.getByText('Claude Desktop', { exact: true })).toBeVisible()
    await expect(page.getByText('evil.example', { exact: true })).toBeVisible()
    await expect(page.getByRole('alert')).toContainText('Unverified client')

    // A separate initiator receives the URL and polling secret, but opening it
    // in the victim's browser must not silently give that initiator a session.
    const native = await (await request.post(`${url}/api/account/oauth/github/start`, {
      headers: { origin: url },
      data: { native: true },
    })).json()
    await page.goto(native.url)
    await expect(page.getByRole('heading', { name: 'Autoriser l’application Cairn pour Android' })).toBeVisible()
    await expect(page.getByText(email, { exact: true })).toBeVisible()
    await expect(page.getByRole('alert')).toContainText('Ne confirmez pas un lien reçu')
    const finish = () => request.post(`${url}/api/account/oauth/github/native/finish`, {
      headers: { origin: url },
      data: { challenge: native.challenge, secret: native.secret },
    })
    expect((await finish()).status()).toBe(202)
    expect((await request.get(`${url}/api/account/session`).then(response => response.json())).authenticated).toBe(false)
    await page.getByRole('button', { name: 'Autoriser sur cet appareil', exact: true }).click()
    await expect(page.getByText('Connexion réussie. Revenez dans l’application Cairn.')).toBeVisible()
    const nativeSession = await finish()
    expect(nativeSession.status()).toBe(200)
    expect((await nativeSession.json()).account).toEqual(first.account)
    expect((await finish()).status()).toBe(401)
  }
  finally {
    child.kill('SIGTERM')
    await exited
    await new Promise<void>(resolve => provider.close(() => resolve()))
  }
})
