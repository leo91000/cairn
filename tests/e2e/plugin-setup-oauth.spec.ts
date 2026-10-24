import type { Server } from 'node:http'
import { once } from 'node:events'
import { createServer } from 'node:http'
import path from 'node:path'
import process from 'node:process'
import {
  test as base,
  expect,
  signIn,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

interface Provider {
  server: Server
  exchanges: Array<Record<string, string>>
  failures: string[]
  verifications: number
}

const test = base.extend<object, { pluginProvider: Provider }>({
  // Playwright requires a destructuring pattern for fixtures without dependencies.
  // eslint-disable-next-line no-empty-pattern
  pluginProvider: [async ({}, use) => {
    const exchanges: Provider['exchanges'] = []
    const failures: string[] = []
    let verifications = 0
    const server = createServer(async (request, response) => {
      let body = ''
      for await (const chunk of request)
        body += chunk
      response.setHeader('content-type', 'application/json')
      if (request.url === '/token') {
        const fields = Object.fromEntries(new URLSearchParams(body))
        exchanges.push(fields)
        if (fields.grant_type !== 'authorization_code' || fields.code !== 'owner-consent-code'
          || fields.client_id !== 'root-client-fixture' || (fields.code_verifier?.length ?? 0) < 43) {
          failures.push('Unexpected authorization-code exchange')
          response.writeHead(400).end('{}')
          return
        }

        response.end(JSON.stringify({
          access_token: 'root-access-fixture',
          refresh_token: 'root-refresh-fixture',
          token_type: 'Bearer',
          expires_in: 3600,
          scope: 'app:read',
        }))
      }
      else if (request.url === '/verify') {
        verifications++
        if (request.headers.authorization !== 'Bearer root-access-fixture') {
          failures.push('Missing root authorization')
          response.writeHead(401).end('{}')
          return
        }

        response.end(JSON.stringify({ ok: true }))
      }
      else {
        response.writeHead(404).end('{}')
      }
    })
    server.listen(0, '127.0.0.1')
    await once(server, 'listening')
    const provider: Provider = {
      server,
      exchanges,
      failures,
      get verifications() { return verifications },
    }
    const origin = `http://127.0.0.1:${(server.address() as { port: number }).port}`
    const overrides = {
      CAIRN_TEST_BINARY: path.resolve('target/debug/examples/worker_fixture'),
      CAIRN_FIXTURE_PLUGIN_HTTP_ORIGIN: origin,
      CAIRN_FIXTURE_BEACON_AUTHORITY: 'true',
    }
    const previous = Object.fromEntries(Object.keys(overrides).map(key => [key, process.env[key]]))
    Object.assign(process.env, overrides)
    try {
      await use(provider)
    }
    finally {
      for (const [key, value] of Object.entries(previous)) {
        if (value === undefined)
          delete process.env[key]
        else
          process.env[key] = value
      }

      await new Promise<void>(resolve => provider.server.close(() => resolve()))
    }
  }, { scope: 'worker', auto: true }],
})

test('owner root consent returns through the real callback and is saved before activation', async ({ page, workspace, pluginProvider }) => {
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'root-consent-complete',
    name: 'Root consent',
    version: '1',
    apiVersion: 2,
    capabilities: ['http', 'resources'],
    hosts: ['provider.example.test'],
    instanceForm: true,
    credential: {
      label: 'Service account',
      header: 'authorization',
      prefix: 'Bearer ',
      oauth: {
        authorizationUrl: 'https://provider.example.test/authorize',
        tokenUrl: 'https://provider.example.test/token',
        clientIdSetting: 'clientId',
        scopes: ['app:read'],
        requireRefreshToken: true,
      },
    },
    settings: [{
      key: 'clientId',
      title: 'Client ID',
      type: 'string',
      required: true,
    }],
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
  }
  const source = `return { form = function(event)
    if not event.credential then return {errors = {credential = 'Sign in to verify this app.'}} end
    local result = cairn.http.request({url = 'https://provider.example.test/verify', auth = true})
    assert(result.json.ok == true, 'The app is unavailable')
    return {view = {{type = 'text', text = 'Root account verified'}}}
  end }`
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source })
  await workspace.api(`/api/plugins/packages/${manifest.id}/approve`, 'POST', { version: '1', digest: installed.digest })
  await page.goto(workspacePath(`/plugins/${manifest.id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'Add instance', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Root consent' })
  await setup.getByLabel('Client ID', { exact: true }).fill('root-client-fixture')
  await setup.getByRole('button', { name: 'Save setup', exact: true }).click()
  await setup.getByRole('button', { name: 'Sign in with Root consent', exact: true }).click()
  const authorize = setup.getByRole('link', { name: 'Open sign-in page', exact: true })
  await expect(authorize).toBeVisible()
  const consent = new URL((await authorize.getAttribute('href'))!)
  const callback = new URL(consent.searchParams.get('redirect_uri')!)
  callback.searchParams.set('state', consent.searchParams.get('state')!)
  callback.searchParams.set('code', 'owner-consent-code')
  const popup = await page.context().newPage()
  await popup.goto(callback.toString())
  await expect(popup.getByRole('status').filter({ hasText: 'Signed in. Return to the account editor' })).toBeVisible()
  await expect(setup.getByText('Sign-in complete. Save setup to keep this account.', { exact: true })).toBeVisible()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeDisabled()
  const patch = page.waitForRequest(request => request.method() === 'PATCH' && request.url().includes(`/plugins/${manifest.id}/setup-drafts/`))
  await setup.getByRole('button', { name: 'Save setup', exact: true }).click()
  const captured = (await patch).postDataJSON()
  expect(captured.credentialFlow).toBeTruthy()
  expect(captured.token).toBeUndefined()
  await expect(setup.getByText('Root account verified', { exact: true })).toBeVisible()
  await expect(setup.getByRole('button', { name: 'Activate instance', exact: true })).toBeEnabled()
  await setup.getByRole('button', { name: 'Activate instance', exact: true }).click()
  await expect(setup).not.toBeVisible()
  const catalog = await workspace.api('/api/plugins')
  const saved = catalog.connections.find((item: any) => item.pluginId === manifest.id)
  expect(saved).toBeDefined()
  expect(saved.agentIds).toEqual([])
  expect(JSON.stringify(catalog)).not.toContain('root-access-fixture')
  expect(pluginProvider.exchanges).toHaveLength(1)
  expect(pluginProvider.verifications).toBeGreaterThanOrEqual(2)
  expect(pluginProvider.failures).toEqual([])
  await popup.close()
})

test('closing setup while a previous sign-in is retiring cannot start another consent', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const manifest = {
    id: 'root-retiring-consent',
    name: 'Retiring consent',
    version: '1',
    apiVersion: 2,
    capabilities: ['http', 'resources'],
    hosts: ['provider.example.test'],
    credential: {
      label: 'Service account',
      header: 'authorization',
      prefix: 'Bearer ',
      oauth: {
        authorizationUrl: 'https://provider.example.test/authorize',
        tokenUrl: 'https://provider.example.test/token',
        clientIdSetting: 'clientId',
        scopes: ['app:read'],
      },
    },
    settings: [{
      key: 'clientId',
      title: 'Client ID',
      type: 'string',
      required: true,
    }],
    resources: [{ name: 'workspace', title: 'Workspaces', singular: 'Workspace' }],
  }
  const installed = await workspace.api('/api/plugins/packages', 'POST', { manifest, source: 'return {}' })
  await workspace.api(`/api/plugins/packages/${manifest.id}/approve`, 'POST', { version: '1', digest: installed.digest })
  const draft = await workspace.api(`/api/plugins/${manifest.id}/setup-drafts`, 'POST', { name: 'Waiting root', settings: { clientId: 'root-client-fixture' } })
  await page.goto(workspacePath(`/plugins/${manifest.id}`))
  await signIn(page)
  await page.getByRole('button', { name: 'Continue Waiting root', exact: true }).click()
  const setup = page.getByRole('dialog', { name: 'Set up Retiring consent' })
  const connect = setup.getByRole('button', { name: 'Sign in with Retiring consent', exact: true })
  await connect.click()
  const authorize = setup.getByRole('link', { name: 'Open sign-in page', exact: true })
  await expect(authorize).toBeVisible()
  const consent = new URL((await authorize.getAttribute('href'))!)
  const callback = new URL(consent.searchParams.get('redirect_uri')!)
  callback.searchParams.set('state', consent.searchParams.get('state')!)
  callback.searchParams.set('error', 'access_denied')
  const popup = await page.context().newPage()
  await popup.goto(callback.toString())
  await expect(popup.getByRole('alert')).toContainText('Sign-in was declined')
  await expect(connect).toBeEnabled()
  let enter!: () => void
  let release!: () => void
  const entered = new Promise<void>((resolve) => {
    enter = resolve
  })
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(`**/plugins/${manifest.id}/oauth/*`, async (route) => {
    if (route.request().method() !== 'DELETE')
      return route.continue()
    const response = await route.fetch()
    enter()
    await gate
    await route.fulfill({ response })
  })
  try {
    await connect.click()
    await entered
    await page.keyboard.press('Escape')
    await expect(setup).not.toBeVisible()
    const late = page.waitForRequest(request => request.method() === 'POST' && request.url().includes(`/setup-drafts/${draft.id}/oauth`), { timeout: 1000 }).catch(() => null)
    release()
    expect(await late).toBeNull()
  }
  finally {
    release()
    await popup.close()
  }
})
