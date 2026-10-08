import { Buffer } from 'node:buffer'
import { readFile } from 'node:fs/promises'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  workspacePath,
} from './fixtures'

async function signIn(page: import('@playwright/test').Page) {
  await page.goto(workspacePath('/agents'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('button', { name: 'New agent', exact: true })).toBeVisible()
}

test('uploads a persistent portrait, preserves it on edits and shows it in conversations', async ({ page, workspace }, testInfo) => {
  await signIn(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name', { exact: true }).fill('Portrait reviewer')
  await expect(dialog).toContainText('Save your agent to upload a portrait.')
  await expect(dialog).toContainText('Connect and enable a Codex account in Connections')
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await page.getByRole('button', { name: 'Edit Portrait reviewer', exact: true }).click()
  const portrait = dialog.getByRole('region', { name: 'Agent portrait' })
  await portrait.getByLabel('Upload agent portrait').setInputFiles('tests/fixtures/artifacts/thumbnail.png')
  await expect(portrait.locator('img')).toBeVisible()
  await expect(portrait.locator('img')).toHaveJSProperty('naturalWidth', 256)
  const source = await portrait.locator('img').getAttribute('src')
  // A malformed image must leave the existing portrait in place.
  await portrait.getByLabel('Upload agent portrait').setInputFiles({ name: 'invalid.png', mimeType: 'image/png', buffer: Buffer.from('not an image') })
  await expect(portrait.getByRole('alert')).toContainText('Choose a valid PNG')
  await expect(portrait.locator('img')).toHaveAttribute('src', source!)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    await portrait.scrollIntoViewIfNeeded()
    await page.screenshot({ path: testInfo.outputPath(`portrait-editor-${colorScheme}.png`) })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  }

  await dialog.getByLabel('Description', { exact: true }).fill('A new role without changing identity')
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  const card = page.locator('article').filter({ has: page.getByRole('button', { name: 'Portrait reviewer', exact: true }) })
  await expect(card.locator('img')).toHaveAttribute('src', source!)
  await workspace.restart()
  await page.reload()
  await expect(card.locator('img')).toHaveJSProperty('naturalWidth', 256)
  await card.getByRole('link', { name: 'Start chat' }).click()
  await expect(page.locator('.agent-avatar img').first()).toBeVisible()
  await expect(page.locator('.agent-avatar img').first()).toHaveAttribute('src', source!)
})

test('shows generation progress, refreshes the portrait and falls back if an image fails to load', async ({ page, workspace }) => {
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Generated identity' })
  await page.route('**/api/agent-avatars', route => route.fulfill({ json: { configured: true } }))
  // UI behavior uses a controlled completion; real provider requests and races are
  // covered by Rust HTTP + Codex protocol tests with synthetic subscription accounts.
  await page.route(`**/api/agents/${agent.id}/avatar/generate`, async (route) => {
    const pending = { ...agent, avatar: { status: 'generating', revision: 'fixture-revision', url: null } }
    workspace.service.store.put('agents', pending)
    await route.fulfill({ json: pending })
  })
  await signIn(page)
  await page.getByRole('button', { name: 'Edit Generated identity', exact: true }).click()
  const portrait = page.getByRole('region', { name: 'Agent portrait' })
  await portrait.getByRole('button', { name: 'Generate portrait', exact: true }).click()
  await expect(portrait.getByRole('status')).toContainText('Creating a portrait')
  await expect(portrait.getByRole('button', { name: 'Generate portrait', exact: true })).toBeDisabled()
  await expect(portrait.getByRole('button', { name: 'Upload image', exact: true })).toBeEnabled()
  // Complete outside the editor so its pending state can only settle through
  // the app's background refresh, as it does after provider generation.
  const session = await (await page.request.get('/api/account/session')).json()
  const completed = await page.request.put(`/api/installations/${workspace.installationId}/api/agents/${agent.id}/avatar`, {
    headers: { 'Origin': workspace.url, 'X-CSRF-Token': session.csrf, 'Content-Type': 'image/png' },
    data: await readFile('tests/fixtures/artifacts/thumbnail.png'),
  })
  expect(completed.ok()).toBe(true)
  await expect(portrait.getByRole('status')).toHaveCount(0)
  await expect(portrait.getByRole('button', { name: 'Regenerate portrait', exact: true })).toBeEnabled()
  await expect(portrait.locator('img')).toHaveJSProperty('naturalWidth', 256)
  await page.route(`**/api/agents/${agent.id}/avatar?**`, route => route.fulfill({ status: 404, body: '' }))
  await page.reload()
  const card = page.locator('article').filter({ has: page.getByRole('button', { name: 'Generated identity', exact: true }) })
  await expect(card.locator('.agent-avatar')).toHaveText('G')
  await expect(card.locator('img')).toHaveCount(0)
})
