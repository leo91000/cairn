import type { Route } from '@playwright/test'
import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('Gmail connects two explicit mailboxes with OAuth and saves no browser token', async ({ page, workspace }, testInfo) => {
  const capture = (name: string, target: { screenshot: (options: { path: string, animations: 'disabled' }) => Promise<unknown> } = page) =>
    target.screenshot({ path: testInfo.outputPath(`gmail-${name}.png`), animations: 'disabled' })
  await useRelayForHttpMocks(page)
  const catalog = await workspace.api('/api/plugins')
  const gmail = catalog.packages.find((item: { id: string }) => item.id === 'gmail').manifest
  expect(gmail.brand?.color).toBe('#EA4335')
  expect(gmail.brand?.path).toBeTruthy()
  const connections: Record<string, unknown>[] = []
  const starts: Record<string, any>[] = []
  const saves: Record<string, any>[] = []
  const callbackUrl = 'https://cairn.example/plugins/oauth/callback'
  await page.route('**/api/plugins/packages/gmail', route => route.fulfill({
    json: {
      id: 'gmail',
      builtin: true,
      status: 'active',
      active: '1.0.0',
      manifest: { ...gmail, instanceView: false },
      capabilities: [],
      instanceReads: true,
      accessReads: true,
    },
  }))
  await page.route('**/api/plugins/packages/gmail/connections', route => route.fulfill({ json: { items: connections } }))
  await page.route('**/api/plugins/instance-access', route => route.fulfill({ json: { items: [] } }))
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
  const logo = page.getByRole('heading', { name: 'Gmail', exact: true }).locator('../..').locator('svg')
  await expect(logo).toBeVisible()
  await expect(logo.locator('path')).toHaveAttribute('d', gmail.brand.path)
  await expect(logo.locator('..')).toHaveCSS('background-color', 'rgb(234, 67, 53)')

  // The page says before anything else that Gmail needs a Google Cloud app.
  const first = page.getByRole('region', { name: 'Set up your first account' })
  await expect(first).toContainText('Google Cloud app you own')
  await expect(first.getByText('Needs your own app')).toBeVisible()
  await capture('first-setup')
  await first.getByRole('button', { name: 'Start setup' }).click()

  // First mailbox: the Google Cloud app is created, then the permissions, then the sign-in.
  let editor = page.getByRole('dialog', { name: 'New account' })
  await expect(editor.getByRole('heading', { name: 'Google Cloud app' })).toBeVisible()
  await expect(editor.getByRole('link', { name: /Open API Library/ })).toHaveAttribute('href', 'https://console.cloud.google.com/apis/library/gmail.googleapis.com')
  await expect(editor.getByRole('textbox', { name: 'Callback URL' })).toHaveValue(callbackUrl)
  await expect(editor.getByRole('note')).toContainText('seven days')
  await expect(editor.getByRole('note')).toContainText('External apps in Testing')
  await expect(editor.getByRole('link', { name: /Open Audience/ })).toHaveAttribute('href', 'https://console.cloud.google.com/auth/audience')
  await expect(editor.getByRole('button', { name: 'Continue' })).toBeDisabled()
  await capture('step-app', editor)
  await editor.getByLabel('Google OAuth client ID').fill('public-client')
  await editor.getByLabel('Google OAuth client secret').fill('synthetic-client-secret')
  await editor.getByRole('button', { name: 'Continue' }).click()

  await expect(editor.getByRole('heading', { name: 'Permissions' })).toBeVisible()
  await expect(editor.getByRole('radio', { name: /Read only/ })).toBeChecked()
  await editor.getByRole('radio', { name: /Read, draft and send/ }).check()
  await editor.getByText('Fine-tune').click()
  await expect(editor.getByRole('checkbox', { name: /Send messages/ })).toBeChecked()
  await expect(editor.getByRole('checkbox', { name: /Permanently delete mail.*High risk/ })).not.toBeChecked()
  await capture('step-permissions', editor)
  await editor.getByRole('button', { name: 'Continue' }).click()

  await expect(editor.getByLabel('Google authorization', { exact: true })).toHaveCount(0)
  await expect(editor.getByRole('button', { name: 'Continue' })).toBeDisabled()
  await editor.getByRole('button', { name: 'Sign in with Google', exact: true }).click()
  await expect(editor.getByLabel('Verification code', { exact: true })).toHaveCount(0)
  await expect(editor.getByRole('link', { name: 'Open sign-in' })).toHaveAttribute('href', /accounts\.google\.com/)
  await expect(editor.getByText('Connected as alice@example.test')).toBeVisible({ timeout: 10000 })
  await capture('step-sign-in', editor)
  await editor.getByRole('button', { name: 'Continue' }).click()

  // The last step grants agents and names the account after the verified mailbox.
  await expect(editor.getByRole('heading', { name: 'Agents' })).toBeVisible()
  await expect(editor.getByLabel('Name', { exact: true })).toHaveValue('alice@example.test')
  await editor.getByRole('checkbox', { name: /Main agent/ }).check()
  await capture('step-agents', editor)
  await editor.getByRole('button', { name: 'Save account', exact: true }).click()
  await expect(editor).toHaveCount(0)

  // Second mailbox: the same Google Cloud app is offered, so only permissions and sign-in remain.
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  editor = page.getByRole('dialog', { name: 'New account' })
  await expect(editor.getByRole('radio', { name: /Use the same one as alice@example\.test/ })).toBeChecked()
  await expect(editor.getByLabel('Google OAuth client ID')).toHaveCount(0)
  await capture('second-account-reuse', editor)
  await editor.getByRole('button', { name: 'Continue' }).click()
  await editor.getByRole('button', { name: 'Continue' }).click()
  await editor.getByRole('button', { name: 'Sign in with Google', exact: true }).click()
  await expect(editor.getByText('Connected as bob@example.test')).toBeVisible({ timeout: 10000 })
  // Other permissions after signing in need a new sign-in, which the installation would require.
  await editor.getByRole('button', { name: 'Back', exact: true }).click()
  await editor.getByRole('radio', { name: /Read, draft and send/ }).check()
  await editor.getByRole('button', { name: 'Continue' }).click()
  await expect(editor.getByText('Connected as bob@example.test')).toHaveCount(0)
  await expect(editor.getByRole('button', { name: 'Continue' })).toBeDisabled()
  await editor.getByRole('button', { name: 'Sign in with Google', exact: true }).click()
  await expect(editor.getByText('Connected as bob@example.test')).toBeVisible({ timeout: 10000 })
  await editor.getByRole('button', { name: 'Continue' }).click()
  await expect(editor.getByLabel('Name', { exact: true })).toHaveValue('bob@example.test')
  await editor.getByRole('button', { name: 'Save account', exact: true }).click()
  await expect(editor).toHaveCount(0)

  expect(starts).toHaveLength(3)
  expect(starts[0]).toMatchObject({
    settings: {
      clientId: 'public-client',
      allowSend: true,
      allowDrafts: true,
      allowDelete: false,
    },
    secrets: { clientSecret: 'synthetic-client-secret' },
  })
  expect(starts[0]).not.toHaveProperty('sharedFrom')
  // The second sign-in names the first account; its secret never travels through the browser.
  expect(starts[1]).toMatchObject({ sharedFrom: connections[0]!.id, settings: { allowSend: false } })
  expect(starts[1].secrets).toEqual({})
  expect(starts[2]).toMatchObject({ sharedFrom: connections[0]!.id, settings: { allowSend: true } })
  expect(saves.map(value => value.name)).toEqual(['alice@example.test', 'bob@example.test'])
  expect(saves.map(value => value.settings.email)).toEqual(['alice@example.test', 'bob@example.test'])
  expect(saves.map(value => value.credentialFlow)).toEqual(['flow-1', 'flow-3'])
  expect(saves[1].sharedFrom).toBe(connections[0]!.id)
  // Creation grants the agents ticked on the last step, and only those.
  const main = (await workspace.api('/api/agents')).find((agent: { name: string }) => agent.name === 'Main agent')
  expect(saves[0].agentIds).toEqual([main.id])
  expect(saves[1]).not.toHaveProperty('agentIds')
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
