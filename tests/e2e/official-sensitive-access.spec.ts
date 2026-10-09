import type { Page } from '@playwright/test'
import { createHash, randomUUID } from 'node:crypto'
import { join } from 'node:path'
import process from 'node:process'
import { test as base, expect } from '@playwright/test'
import { executeOfficialSql, expireAccountProof, officialRelayFixture } from './official-relay-fixture'

type InstallationFixture = Awaited<ReturnType<typeof officialRelayFixture>> & { email: string, installationId: string, databaseUrl: string }

const test = base.extend<{ installation: InstallationFixture }>({
  installation: async ({ page }, use) => {
    const fixture = await officialRelayFixture(4394)
    const { root, url, messages } = fixture
    const schema = `sensitive_${randomUUID().replaceAll('-', '')}`
    const database = new URL(process.env.CAIRN_BEACON_TEST_DATABASE_URL!)
    executeOfficialSql(database, `CREATE SCHEMA ${schema}`)
    database.searchParams.set('options', `-csearch_path=${schema}`)
    fixture.official(database.toString())
    try {
      await expect.poll(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false)).toBe(true)
      const email = `sensitive-${Date.now()}@example.test`
      await page.goto(url)
      await page.getByLabel('Email address').fill(email)
      await page.getByRole('button', { name: 'Send code', exact: true }).click()
      await expect.poll(() => messages.length).toBe(1)
      await page.getByLabel('Email code').fill(messages[0]!.match(/\b\d{8}\b/)![0])
      await page.getByRole('button', { name: 'Sign in', exact: true }).click()
      await page.getByRole('button', { name: 'Add an installation', exact: true }).click()
      const code = await page.getByLabel('Installation claim code').inputValue()
      fixture.start('target/debug/cairn', {
        DATA_DIR: join(root, 'data'),
        AGENT_HOME: join(root, 'home'),
        WORKSPACE_ROOTS: root,
        NODE_ENV: 'test',
        WORKER_ENABLED: 'false',
        PORT: '0',
        CAIRN_BEACON_ORIGIN: url,
        CAIRN_INSTALLATION_CLAIM_CODE: code,
        CAIRN_INSTALLATION_NAME: 'Security installation',
      })
      await expect(async () => {
        await page.getByRole('button', { name: 'Refresh installations', exact: true }).click()
        await expect(page).toHaveURL(/\/installations\/[^/]+\/$/)
      }).toPass()
      await use({
        ...fixture,
        email,
        databaseUrl: database.toString(),
        installationId: page.url().split('/')[4]!,
      })
    }
    finally {
      await fixture.close()
      executeOfficialSql(new URL(process.env.CAIRN_BEACON_TEST_DATABASE_URL!), `DROP SCHEMA ${schema} CASCADE`)
    }
  },
})

test.describe.configure({ timeout: 120000 })

function expireProof(installation: InstallationFixture) {
  expireAccountProof(new URL(installation.databaseUrl), installation.email)
}

async function confirmIdentity(page: Page, installation: InstallationFixture, confirm = page.getByRole('button', { name: 'Confirm identity', exact: true })) {
  await expect(confirm).toBeVisible()
  await confirm.click()
  const previousEmails = installation.messages.length
  await page.getByRole('button', { name: 'Send confirmation code', exact: true }).click()
  await expect.poll(() => installation.messages.slice(previousEmails).find(message => /\b\d{8}\b/.test(message))).toBeTruthy()
  const proof = installation.messages.slice(previousEmails).find(message => /\b\d{8}\b/.test(message))!
  await page.getByLabel('Confirmation code').fill(proof.match(/\b\d{8}\b/)![0])
  await page.getByRole('button', { name: 'Verify confirmation code', exact: true }).click()
}

test('personal token confirmation preserves its name and permissions without creating automatically', async ({ page, installation }) => {
  const { url, installationId } = installation
  await page.goto(`${url}/installations/${installationId}/settings/installation`)
  await page.getByRole('button', { name: 'New token', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name', { exact: true }).fill('Confirmed personal client')
  await dialog.getByLabel('Start and cancel configured tasks').check()
  expireProof(installation)
  await dialog.getByRole('button', { name: 'Create token', exact: true }).click()
  await expect(dialog.getByRole('alert')).toContainText('Confirm your identity')
  await confirmIdentity(page, installation)
  await expect(dialog.getByLabel('Name', { exact: true })).toHaveValue('Confirmed personal client')
  await expect(dialog.getByLabel('Start and cancel configured tasks')).toBeChecked()
  await expect(dialog).not.toContainText('This token is shown once')
  const grants = await (await page.request.get(`${url}/api/installations/${installationId}/tokens`)).json()
  expect(grants).toEqual([])
  await dialog.getByRole('button', { name: 'Create token', exact: true }).click()
  await expect(dialog).toContainText('This token is shown once')
  await dialog.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await expect(page.getByText('Confirmed personal client', { exact: true })).toBeVisible()
})

test('MCP confirmation preserves the selected installation and waits for explicit consent', async ({ page, installation }) => {
  const { url, installationId } = installation
  const callback = 'http://localhost:9594/callback'
  await page.route(`${callback}**`, route => route.fulfill({ contentType: 'text/html', body: '<p>Authorization received</p>' }))
  const client = await (await page.request.post(`${url}/oauth/register`, {
    data: { client_name: 'Confirmed MCP client', redirect_uris: [callback] },
  })).json()
  const verifier = 'a'.repeat(43)
  const parameters = new URLSearchParams({
    client_id: client.client_id,
    redirect_uri: callback,
    response_type: 'code',
    code_challenge_method: 'S256',
    code_challenge: createHash('sha256').update(verifier).digest('base64url'),
    scope: 'read',
    state: 'preserved-state',
    resource: `${url}/mcp`,
  })
  await page.goto(`${url}/oauth/authorize?${parameters}`)
  await page.getByRole('combobox', { name: 'Installation', exact: true }).click()
  await page.getByRole('option', { name: 'Security installation', exact: true }).click()
  const consentUrl = page.url()
  expect(Object.fromEntries(new URL(consentUrl).searchParams)).toEqual(Object.fromEntries(parameters))
  expireProof(installation)
  await page.getByRole('button', { name: 'Allow access', exact: true }).click()
  await expect(page.getByRole('alert').filter({ hasText: 'Confirm your identity' })).toBeVisible()
  await confirmIdentity(page, installation)
  await expect(page.getByRole('combobox', { name: 'Installation', exact: true })).toHaveValue('Security installation')
  await expect(page).toHaveURL(consentUrl)
  expect(await (await page.request.get(`${url}/api/installations/${installationId}/tokens`)).json()).toEqual([])
  await page.getByRole('button', { name: 'Allow access', exact: true }).click()
  await expect(page).toHaveURL(/callback\?state=preserved-state&code=/)
  const code = new URL(page.url()).searchParams.get('code')!
  const exchanged = await page.request.post(`${url}/oauth/token`, {
    form: {
      grant_type: 'authorization_code',
      client_id: client.client_id,
      redirect_uri: callback,
      code,
      code_verifier: verifier,
      resource: `${url}/mcp`,
    },
  })
  expect(exchanged.status()).toBe(200)
})

test('confirming identity keeps other devices signed in until explicit revocation', async ({ page, browser, installation }) => {
  const { url, email, messages } = installation
  const otherDevice = await browser.newContext({ userAgent: 'Lost phone browser' })
  try {
    expireProof(installation)
    const other = await otherDevice.newPage()
    await other.goto(url)
    await other.getByLabel('Email address').fill(email)
    const previousEmails = messages.length
    await other.getByRole('button', { name: 'Send code', exact: true }).click()
    await expect.poll(() => messages.length).toBe(previousEmails + 1)
    await other.getByLabel('Email code').fill(messages.at(-1)!.match(/\b\d{8}\b/)![0])
    await other.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(other).toHaveURL(/\/installations\/[^/]+\/$/)
    await page.getByRole('button', { name: 'Account', exact: true }).click()
    await page.getByRole('menuitem', { name: 'Account settings', exact: true }).click()
    await expect(page.getByRole('heading', { name: 'Account security', exact: true })).toBeVisible()
    await page.getByRole('button', { name: 'Revoke other devices', exact: true }).click()
    await expect(page.getByRole('alert')).toContainText('Confirm your identity')
    await expect(page.getByText('Lost phone browser', { exact: true })).toBeVisible()
    expireProof(installation)
    await confirmIdentity(page, installation, page.getByRole('region', { name: 'Account security', exact: true }).getByRole('button', { name: 'Confirm identity', exact: true }))
    await expect(page.getByRole('heading', { name: 'Active sessions' })).toBeVisible()
    await expect(page.getByText('Lost phone browser', { exact: true })).toBeVisible()
    await other.reload()
    await expect(other).toHaveURL(/\/installations\/[^/]+\/$/)
    await expect(other.getByRole('region', { name: 'Cairn account' })).toHaveCount(0)
    await page.getByRole('button', { name: 'Revoke other devices', exact: true }).click()
    await expect(page.getByText('Lost phone browser', { exact: true })).toHaveCount(0)
    await other.reload()
    await expect(other.getByLabel('Email address')).toBeVisible()
  }
  finally {
    await otherDevice.close()
  }
})
