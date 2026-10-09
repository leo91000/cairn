import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { expect, test } from '@playwright/test'

test('beacon pages deny framing and enable HSTS only for an HTTPS beacon origin', async ({ page, request }) => {
  const url = 'http://localhost:4398'
  for (const origin of [url, 'https://localhost:4398']) {
    const child = spawn('target/debug/cairn-beacon', [], {
      env: {
        ...process.env,
        CAIRN_BEACON_DATABASE_URL: process.env.CAIRN_BEACON_TEST_DATABASE_URL,
        CAIRN_BEACON_ORIGIN: origin,
        CAIRN_BEACON_LISTEN: '127.0.0.1:4398',
        CAIRN_BEACON_EMAIL_KEY: 'fixture-only',
        CAIRN_BEACON_EMAIL_FROM: 'Cairn <cairn@example.test>',
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
      }).toBe(true)

      // The transport is loopback HTTP, as behind a TLS-terminating proxy.
      for (const route of ['/', '/index.html', '/installations/unavailable/agents']) {
        const response = await request.get(`${url}${route}`)
        expect(response.status()).toBe(200)
        expect(response.headers()['content-security-policy']).toContain('frame-ancestors \'none\'')
        expect(response.headers()['x-frame-options']).toBe('DENY')
        expect(response.headers()['strict-transport-security']).toBe(origin.startsWith('https:') ? 'max-age=31536000' : undefined)
      }

      await page.goto(url)
      await expect(page.getByLabel('Email address')).toBeVisible()
      const framingBlocked = page.waitForEvent('console', {
        predicate: message => /frame-ancestors|X-Frame-Options/i.test(message.text()),
      })
      await page.setContent(`<iframe src="${url}/"></iframe>`)
      await framingBlocked
      await expect(page.frameLocator('iframe').getByLabel('Email address')).toHaveCount(0)
    }
    finally {
      child.kill('SIGTERM')
      await exited
    }
  }
})

test('email sign-in works after a third party exhausts their challenge, persists and signs out', async ({ page, context, request }) => {
  const messages: Array<{ to: string[], text: string }> = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body))
    response.writeHead(200, { 'content-type': 'application/json' }).end('{"id":"fixture"}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const mailPort = (mail.address() as { port: number }).port
  const url = 'http://localhost:4398'
  const child = spawn('target/debug/cairn-beacon', [], {
    env: {
      ...process.env,
      CAIRN_BEACON_DATABASE_URL: process.env.CAIRN_BEACON_TEST_DATABASE_URL,
      CAIRN_BEACON_ORIGIN: url,
      CAIRN_BEACON_LISTEN: '127.0.0.1:4398',
      CAIRN_BEACON_EMAIL_ENDPOINT: `http://127.0.0.1:${mailPort}/emails`,
      CAIRN_BEACON_EMAIL_KEY: 'fixture-only',
      CAIRN_BEACON_EMAIL_FROM: 'Cairn <cairn@example.test>',
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
    }).toBe(true)
    const cdp = await context.newCDPSession(page)
    await cdp.send('WebAuthn.enable')
    const { authenticatorId } = await cdp.send('WebAuthn.addVirtualAuthenticator', {
      options: {
        protocol: 'ctap2',
        transport: 'internal',
        hasResidentKey: true,
        hasUserVerification: true,
        isUserVerified: true,
        automaticPresenceSimulation: true,
      },
    })
    const email = `browser-${Date.now()}@example.test`
    // A third party knows the address and gets a challenge, but cannot read its inbox.
    const unsolicited = await request.post(`${url}/api/account/email-code`, {
      headers: { origin: url },
      data: { email },
    })
    expect(unsolicited.status()).toBe(202)
    const attacker = await unsolicited.json()
    for (let attempt = 0; attempt < 5; attempt++) {
      const rejected = await request.post(`${url}/api/account/verify`, {
        headers: { origin: url },
        data: { challenge: attacker.challenge, code: 'wrong' },
      })
      expect(rejected.status()).toBe(401)
    }

    await expect.poll(() => messages.length).toBe(1)

    await page.goto(url)
    await page.getByLabel('Email address').fill(email)
    await page.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect(page.getByLabel('Email code')).toBeVisible()
    await expect.poll(() => messages.length).toBe(1)
    await page.getByLabel('Email code').fill('wrong')
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Invalid or expired code')
    await page.getByLabel('Email code').fill(messages[0]!.text.match(/\b\d{8}\b/)![0])
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.reload()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
    const claimCode = await page.getByLabel('Installation claim code').inputValue()
    await expect(page.getByLabel('Installation command', { exact: true })).toHaveValue(`curl -fsSL '${url}/install.sh' | sudo bash -s -- --claim-code '${claimCode}'`)
    await expect(page.getByRole('button', { name: 'Sign-in methods', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
    await page.getByLabel('Passkey name').fill('Laptop')
    await page.getByRole('button', { name: 'Add passkey', exact: true }).click()
    await expect(page.getByText('Laptop', { exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Back to installations' }).click()
    const secondDevice = await context.browser()!.newContext({ userAgent: 'Lost phone browser' })
    try {
      const otherPage = await secondDevice.newPage()
      const otherCdp = await secondDevice.newCDPSession(otherPage)
      await otherCdp.send('WebAuthn.enable')
      const otherAuthenticator = await otherCdp.send('WebAuthn.addVirtualAuthenticator', {
        options: {
          protocol: 'ctap2',
          transport: 'internal',
          hasResidentKey: true,
          hasUserVerification: true,
          isUserVerified: true,
          automaticPresenceSimulation: true,
        },
      })
      const credentials = await cdp.send('WebAuthn.getCredentials', { authenticatorId })
      for (const credential of credentials.credentials)
        await otherCdp.send('WebAuthn.addCredential', { authenticatorId: otherAuthenticator.authenticatorId, credential })
      await otherPage.goto(url)
      await otherPage.getByRole('button', { name: 'Sign in with a passkey', exact: true }).click()
      await expect(otherPage.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
      // These two virtual authenticators model a synced passkey. Carry forward
      // its updated counter rather than replaying the old cloned credential.
      const updated = await otherCdp.send('WebAuthn.getCredentials', { authenticatorId: otherAuthenticator.authenticatorId })
      for (const credential of updated.credentials) {
        await cdp.send('WebAuthn.removeCredential', { authenticatorId, credentialId: credential.credentialId })
        await cdp.send('WebAuthn.addCredential', { authenticatorId, credential })
      }

      await page.route('**/api/account/sessions', route => route.fulfill({ status: 502, contentType: 'text/html', body: '<h1>Bad gateway</h1>' }))
      await page.getByRole('button', { name: 'Account security', exact: true }).click()
      await expect(page.getByRole('alert')).toHaveText('Unable to update account security.')
      await page.unroute('**/api/account/sessions')
      await page.getByRole('button', { name: 'Back to installations', exact: true }).click()
      await page.getByRole('button', { name: 'Account security', exact: true }).click()
      await expect(page.getByRole('heading', { name: 'Active sessions' })).toBeVisible()
      await expect(page.getByText('This device', { exact: true })).toBeVisible()
      await expect(page.getByText('Lost phone browser', { exact: true })).toBeVisible()
      for (const width of [1440, 390, 320]) {
        await page.setViewportSize({ width, height: 844 })
        expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
      }

      await page.getByRole('button', { name: 'Revoke other devices', exact: true }).click()
      await expect(page.getByText('Lost phone browser', { exact: true })).toHaveCount(0)
      await otherPage.reload()
      await expect(otherPage.getByLabel('Email address')).toBeVisible()
      await page.getByRole('button', { name: 'Back to installations', exact: true }).click()
      await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    }
    finally {
      await secondDevice.close()
    }

    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page.getByRole('button', { name: 'Sign in with a passkey' })).toBeEnabled()
    await page.getByRole('button', { name: 'Sign in with a passkey' }).click()
    await expect(page.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.getByRole('button', { name: 'Sign-in methods', exact: true }).click()
    await page.getByRole('button', { name: 'Remove Laptop' }).click()
    await expect(page.getByText('Laptop', { exact: true })).toHaveCount(0)
    await expect(page.getByRole('button', { name: /Remove Email/ })).toBeDisabled()
    await page.getByRole('button', { name: 'Back to installations' }).click()
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: 844 })
      await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible()
      expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
    }

    const sibling = await context.newPage()
    await sibling.clock.install()
    let expiredChecks = 0
    sibling.on('response', (response) => {
      if (new URL(response.url()).pathname === '/api/installations' && response.status() === 401)
        expiredChecks++
    })
    await sibling.goto(url)
    await expect(sibling.getByRole('heading', { name: 'No installations yet' })).toBeVisible()
    await page.bringToFront()
    await page.getByRole('button', { name: 'Sign out' }).click()
    await expect(page.getByLabel('Email address')).toBeVisible()
    await sibling.bringToFront()
    await expect(sibling.getByLabel('Email address')).toBeVisible()
    const checksAfterRedirect = expiredChecks
    expect(checksAfterRedirect).toBeGreaterThan(0)
    await sibling.clock.runFor(7000)
    expect(expiredChecks).toBe(checksAfterRedirect)
    await sibling.close()
    await page.bringToFront()
    await page.reload()
    await expect(page.getByLabel('Email address')).toBeVisible()
  }
  finally {
    child.kill('SIGTERM')
    await exited
    await new Promise<void>(resolve => mail.close(() => resolve()))
  }
})
