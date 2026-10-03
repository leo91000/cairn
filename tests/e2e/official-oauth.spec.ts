import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('Google and GitHub reuse a verified Leo account and manage its sign-in methods', async ({ page }) => {
  const githubId = Date.now()
  const email = `oauth-browser-${Date.now()}@example.test`
  const provider = createServer(async (request, response) => {
    const url = new URL(request.url!, 'http://localhost')
    response.setHeader('content-type', 'application/json')
    if (url.pathname === '/authorize') {
      const callback = new URL(url.searchParams.get('redirect_uri')!)
      callback.searchParams.set('state', url.searchParams.get('state')!)
      callback.searchParams.set('code', 'verified')
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
    }).toBe(true)
    await page.goto(url)
    await expect(page.getByRole('button', { name: 'Continue with Google' })).toBeVisible()
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
    await page.getByRole('button', { name: 'Continue with GitHub' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    const signedIn = await page.evaluate(() => fetch('/api/account/session').then(response => response.json()))
    expect(signedIn.account).toEqual(first.account)
  }
  finally {
    child.kill('SIGTERM')
    await exited
    await new Promise<void>(resolve => provider.close(() => resolve()))
  }
})
