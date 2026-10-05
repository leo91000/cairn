import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('Google and GitHub reuse an account, manage sign-in methods and preserve claim navigation', async ({ page }) => {
  test.setTimeout(90000)
  let denyNextAuthorization = false
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
      response.end(JSON.stringify({ access_token: 'test-token', token_type: 'Bearer' }))
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
  const child = spawn('target/debug/leo-official', [], {
    env: {
      ...process.env,
      LEO_OFFICIAL_DATABASE_URL: process.env.LEO_OFFICIAL_TEST_DATABASE_URL,
      LEO_OFFICIAL_ORIGIN: url,
      LEO_OFFICIAL_LISTEN: '127.0.0.1:4399',
      LEO_OFFICIAL_EMAIL_ENDPOINT: `${providerUrl}/email`,
      LEO_OFFICIAL_EMAIL_KEY: 'test-only',
      LEO_OFFICIAL_EMAIL_FROM: 'Leo <leo@example.test>',
      LEO_OFFICIAL_GOOGLE_CLIENT_ID: 'google-test',
      LEO_OFFICIAL_GOOGLE_CLIENT_SECRET: 'test-only',
      LEO_OFFICIAL_GOOGLE_AUTHORIZATION_URL: `${providerUrl}/authorize`,
      LEO_OFFICIAL_GOOGLE_TOKEN_URL: `${providerUrl}/token`,
      LEO_OFFICIAL_GOOGLE_USERINFO_URL: `${providerUrl}/google/userinfo`,
      LEO_OFFICIAL_GITHUB_CLIENT_ID: 'github-test',
      LEO_OFFICIAL_GITHUB_CLIENT_SECRET: 'test-only',
      LEO_OFFICIAL_GITHUB_AUTHORIZATION_URL: `${providerUrl}/authorize`,
      LEO_OFFICIAL_GITHUB_TOKEN_URL: `${providerUrl}/token`,
      LEO_OFFICIAL_GITHUB_USERINFO_URL: `${providerUrl}/github/user`,
      LEO_OFFICIAL_GITHUB_EMAILS_URL: `${providerUrl}/github/emails`,
    },
    stdio: ['ignore', 'ignore', 'pipe'],
  })
  const exited = once(child, 'exit')
  let log = ''
  child.stderr.on('data', chunk => log += chunk)
  try {
    await expect.poll(async () => {
      if (child.exitCode !== null)
        throw new Error(`Official service exited: ${log}`)
      return fetch(`${url}/health`).then(response => response.ok).catch(() => false)
    }, { timeout: 30000 }).toBe(true)
    await page.goto(url)
    await expect(page.getByRole('button', { name: 'Continue with Google' })).toBeVisible()
    denyNextAuthorization = true
    await page.getByRole('button', { name: 'Continue with Google' }).click()
    await expect(page.getByRole('alert')).toContainText('Sign-in was cancelled')
    expect((await page.context().cookies(url)).some(cookie => cookie.name === 'leo_oauth')).toBe(false)
    await page.getByRole('button', { name: 'Continue with Google' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await expect(page.getByText(`You’re signed in as ${email}.`)).toBeVisible()
    const first = await page.evaluate(() => fetch('/api/account/session').then(response => response.json()))
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
    await expect(page.getByRole('button', { name: `Remove Google ${email}` })).toBeDisabled()
    await page.getByRole('button', { name: 'Add GitHub' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    const linked = await page.evaluate(() => fetch('/api/account/session').then(response => response.json()))
    expect(linked.account).toEqual(first.account)
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
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
    const installationId = (await claimed.json()).installationId
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
      await page.getByLabel('Installation', { exact: true }).selectOption(installationId)
      await page.getByRole('button', { name: 'Allow access', exact: true }).click()
      await expect(page).toHaveURL(/mcp-test-callback\?state=resume-mcp-consent&code=/)
      expect(new URL(page.url()).origin).toBe(providerUrl)
    }
  }
  finally {
    child.kill('SIGTERM')
    await exited
    await new Promise<void>(resolve => provider.close(() => resolve()))
  }
})
