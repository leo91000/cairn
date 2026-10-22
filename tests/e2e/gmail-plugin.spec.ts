import type { Route } from '@playwright/test'
import gmail from '../../plugins/gmail/manifest.json' with { type: 'json' }
import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('Gmail connects two explicit mailboxes with OAuth and saves no browser token', async ({ page }) => {
  await useRelayForHttpMocks(page)
  const connections: Record<string, unknown>[] = []
  const starts: Record<string, any>[] = []
  const saves: Record<string, any>[] = []
  const callbackUrl = 'https://cairn.example/plugins/oauth/callback'
  await page.route('**/api/plugins', route => route.fulfill({
    json: {
      packages: [{
        id: 'gmail',
        builtin: true,
        status: 'active',
        active: '1.0.0',
        manifest: { ...gmail, instanceView: false },
        versions: [],
      }],
      capabilities: [],
      connections,
    },
  }))
  await page.route('**/api/plugins/gmail/oauth', async (route) => {
    if (route.request().method() === 'GET')
      return route.fulfill({ json: { callbackUrl } })
    starts.push(route.request().postDataJSON())
    return route.fulfill({
      json: {
        id: `flow-${starts.length}`,
        pluginId: 'gmail',
        state: 'pending',
        userCode: '',
        verificationUri: 'https://accounts.google.com/o/oauth2/v2/auth?state=synthetic',
        expiresAt: Date.now() + 600000,
      },
    })
  })
  await page.route('**/api/plugins/gmail/oauth/flow-*', route => route.fulfill({
    json: {
      id: new URL(route.request().url()).pathname.split('/').at(-1),
      pluginId: 'gmail',
      state: 'completed',
      userCode: '',
      verificationUri: '',
      expiresAt: Date.now() + 600000,
    },
  }))
  await page.route('**/api/plugins/gmail/form', async (route: Route) => {
    const input = route.request().postDataJSON()
    const email = input.credentialFlow === 'flow-1' ? 'alice@example.test' : 'bob@example.test'
    await route.fulfill({
      json: {
        values: input.credentialFlow ? { email } : {},
        options: {},
        errors: {},
        view: input.credentialFlow ? [{ type: 'alert', tone: 'success', title: `Connected as ${email}` }] : [],
      },
    })
  })
  await page.route('**/api/plugins/gmail/connections', async (route) => {
    const input = route.request().postDataJSON()
    saves.push(input)
    connections.push({
      ...input,
      id: `00000000-0000-4000-8000-00000000000${saves.length}`,
      pluginId: 'gmail',
      status: { state: 'ready' },
      agentIds: [],
      requests: [],
      activity: [],
      secretKeys: ['clientSecret'],
    })
    await route.fulfill({ json: {} })
  })
  await page.goto(workspacePath('/plugins/gmail'))
  await signIn(page)
  for (const email of ['alice@example.test', 'bob@example.test']) {
    await page.getByRole('button', { name: 'Add account', exact: true }).click()
    const editor = page.getByRole('dialog', { name: 'New account' })
    await expect(editor.getByLabel('Callback URL')).toHaveValue(callbackUrl)
    await expect(editor.getByLabel('Google authorization', { exact: true })).toHaveCount(0)
    await editor.getByLabel('Name', { exact: true }).fill(email)
    await editor.getByLabel('Google OAuth client ID').fill('public-client')
    await editor.getByLabel('Google OAuth client secret').fill('synthetic-client-secret')
    await expect(editor.getByRole('button', { name: 'Save account', exact: true })).toBeDisabled()
    await editor.getByRole('button', { name: 'Sign in with Gmail', exact: true }).click()
    await expect(editor.getByLabel('Verification code', { exact: true })).toHaveCount(0)
    await expect(editor.getByRole('link', { name: 'Open Gmail' })).toHaveAttribute('href', /accounts\.google\.com/)
    await expect(editor.getByText(`Connected as ${email}`)).toBeVisible({ timeout: 10000 })
    await editor.getByRole('button', { name: 'Save account', exact: true }).click()
    await expect(editor).toHaveCount(0)
  }

  expect(starts).toHaveLength(2)
  expect(starts[0]).toMatchObject({ settings: { clientId: 'public-client', allowSend: false }, secrets: { clientSecret: 'synthetic-client-secret' } })
  expect(saves.map(value => value.settings.email)).toEqual(['alice@example.test', 'bob@example.test'])
  expect(saves.map(value => value.credentialFlow)).toEqual(['flow-1', 'flow-2'])
  expect(saves.every(value => !Object.hasOwn(value, 'token'))).toBe(true)
  await expect(page.getByRole('heading', { name: 'alice@example.test', exact: true })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'bob@example.test', exact: true })).toBeVisible()

  const revocationWarning = 'Disconnected locally. Google did not confirm revocation; remove this app from your Google account manually.'
  await page.route('**/api/plugins/gmail/connections/*/disconnect', async (route) => {
    expect(route.request().method()).toBe('POST')
    connections[0]!.enabled = false
    await route.fulfill({ json: { revoked: false, message: revocationWarning } })
  })
  await page.getByRole('button', { name: 'Disconnect', exact: true }).first().click()
  await expect(page.getByRole('status').filter({ hasText: revocationWarning })).toBeVisible()
})

test('OAuth callback removes the code from browser history before posting it', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/plugins'))
  await signIn(page)
  const callbacks: Record<string, unknown>[] = []
  await page.route('**/api/plugins/oauth/callback', async (route) => {
    callbacks.push(route.request().postDataJSON())
    await route.fulfill({ json: { pluginId: 'gmail', flowId: 'flow', state: 'completed' } })
  })
  await page.goto(workspacePath('/plugins/oauth/callback?state=synthetic-state&code=synthetic-code'))
  await expect(page.getByRole('status').filter({ hasText: 'Signed in. Return to the account editor' })).toBeVisible()
  expect(new URL(page.url()).search).toBe('')
  expect(callbacks).toEqual([{ state: 'synthetic-state', code: 'synthetic-code' }])
})

test('central OAuth callback completes without restoring the remembered installation', async ({ page }) => {
  await page.goto(workspacePath('/plugins'))
  await signIn(page)
  const callbacks: Record<string, unknown>[] = []
  await page.route('**/api/plugins/oauth/callback', async (route) => {
    expect(route.request().headers()['x-csrf-token']).toBeTruthy()
    callbacks.push(route.request().postDataJSON())
    await route.fulfill({ json: { pluginId: 'gmail', flowId: 'flow', state: 'completed' } })
  })
  await page.goto('/plugins/oauth/callback?state=destination-state&code=central-code')
  await expect(page.getByRole('status').filter({ hasText: 'Signed in. Return to the account editor' })).toBeVisible()
  expect(new URL(page.url()).pathname).toBe('/plugins/oauth/callback')
  expect(new URL(page.url()).search).toBe('')
  expect(callbacks).toEqual([{ state: 'destination-state', code: 'central-code' }])
  await expect(page.getByRole('banner', { name: 'Current installation' })).toHaveCount(0)
})

test('central OAuth callback keeps a declined response through email sign-in without a browser session', async ({ page }) => {
  const callbacks: Record<string, unknown>[] = []
  const session = {
    authenticated: true,
    csrf: 'synthetic-csrf',
    account: { id: 'owner', email: 'owner@example.test' },
    installations: [{
      id: 'other',
      slug: 'remembered',
      name: 'Other installation',
      role: 'owner',
      online: true,
      updateRequired: false,
    }],
  }
  await page.route('**/api/installations', route => route.fulfill({ json: session.installations }))
  await page.route('**/api/account/session', route => route.fulfill({
    json: {
      authenticated: false,
      csrf: null,
      account: null,
      installations: [],
    },
  }))
  await page.route('**/api/account/email-code', route => route.fulfill({ json: { challenge: 'synthetic-challenge' } }))
  await page.route('**/api/account/verify', route => route.fulfill({ json: session }))
  await page.route('**/api/plugins/oauth/callback', async (route) => {
    expect(route.request().headers()['x-csrf-token']).toBe(session.csrf)
    callbacks.push(route.request().postDataJSON())
    await route.fulfill({ status: 400, json: { error: 'Sign-in was declined. Try again when ready.' } })
  })
  await page.goto('/plugins/oauth/callback?state=destination-state&error=access_denied')
  await expect(page.getByRole('heading', { name: 'Sign in to Cairn' })).toBeVisible()
  expect(new URL(page.url()).search).toBe('')
  expect(callbacks).toEqual([])
  await page.getByLabel('Email address').fill('owner@example.test')
  await page.getByRole('button', { name: 'Send code', exact: true }).click()
  await page.getByLabel('Email code').fill('12345678')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.getByRole('alert')).toContainText('Sign-in was declined')
  expect(callbacks).toEqual([{ state: 'destination-state', error: 'access_denied' }])
  expect(new URL(page.url()).pathname).toBe('/plugins/oauth/callback')
})

test('central OAuth callback resumes after Cairn provider sign-in instead of a remembered workspace', async ({ page }) => {
  let authenticated = false
  const session = {
    authenticated: true,
    csrf: 'synthetic-csrf',
    account: { id: 'owner', email: 'owner@example.test' },
    installations: [{
      id: 'other',
      slug: 'remembered',
      name: 'Other installation',
      role: 'owner',
      online: true,
      updateRequired: false,
    }],
  }
  await page.route('**/api/installations', route => route.fulfill({ json: session.installations }))
  await page.route('**/api/account/session', route => route.fulfill({
    json: authenticated
      ? session
      : {
          authenticated: false,
          csrf: null,
          account: null,
          installations: [],
        },
  }))
  await page.route('**/api/account/options', route => route.fulfill({ json: { google: true, github: false, passkeys: false } }))
  await page.route('**/api/account/oauth/google/start', async (route) => {
    authenticated = true
    await route.fulfill({ json: { url: `${new URL(route.request().url()).origin}/?signed-in=1` } })
  })
  const callbacks: Record<string, unknown>[] = []
  await page.route('**/api/plugins/oauth/callback', async (route) => {
    callbacks.push(route.request().postDataJSON())
    await route.fulfill({ json: { pluginId: 'gmail', flowId: 'flow', state: 'completed' } })
  })
  await page.goto('/plugins/oauth/callback?state=destination-state&code=before-login')
  await expect(page.getByRole('heading', { name: 'Sign in to Cairn' })).toBeVisible()
  expect(new URL(page.url()).search).toBe('')
  await page.getByRole('button', { name: 'Continue with Google', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: 'Signed in. Return to the account editor' })).toBeVisible()
  expect(new URL(page.url()).pathname).toBe('/plugins/oauth/callback')
  expect(new URL(page.url()).search).toBe('')
  expect(callbacks).toEqual([{ state: 'destination-state', code: 'before-login' }])
})
