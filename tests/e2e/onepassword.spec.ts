import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
  workspacePath,
} from './fixtures'

test('1Password service accounts are plugin instances with private tokens and explicit agent grants', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  await page.goto(workspacePath('/connections'))
  await authenticateWorkspace(page)
  // 1Password lives with the plugins only: Connections keeps coding agent accounts and GitHub.
  await expect(page.getByRole('heading', { name: 'Coding agents' })).toBeVisible()
  await expect(page.getByText('1Password')).toHaveCount(0)
  await page.goto(workspacePath('/plugins'))
  await page.getByRole('link', { name: '1Password · Set up' }).click()
  await expect(page).toHaveURL(workspacePath('/plugins/onepassword'))
  await expect(page.getByRole('heading', { name: '1Password', exact: true })).toBeVisible()
  // The package declares what agents get; Cairn renders it.
  const usage = page.getByRole('complementary', { name: 'How agents use it' })
  await expect(usage.getByText('cairn plugin onepassword read --account <account> <reference>')).toBeVisible()
  await expect(usage.getByText('op', { exact: true })).toBeVisible()
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`onepassword-plugin-${width}.png`), fullPage: true, animations: 'disabled' })
  }

  await page.getByRole('button', { name: 'Add service account' }).click()
  let dialog = page.getByRole('dialog', { name: 'New service account' })
  await dialog.getByLabel('Name', { exact: true }).fill('Production secrets')
  const token = dialog.getByLabel('Service account token', { exact: true })
  await expect(token).toHaveAttribute('type', 'password')
  await token.fill('ops_browser_fixture')
  await expect(dialog.getByRole('checkbox', { name: 'Main agent', exact: true })).not.toBeChecked()
  await dialog.getByRole('checkbox', { name: 'Main agent', exact: true }).check()
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 844 })
    await expectSingleScroll(page)
    await dialog.screenshot({ path: testInfo.outputPath(`onepassword-editor-${width}.png`) })
  }

  await dialog.getByRole('button', { name: 'Save service account' }).click()
  await expect(dialog).toHaveCount(0)
  const accounts = async () => (await workspace.api('/api/plugins')).connections.filter((item: { pluginId: string }) => item.pluginId === 'onepassword')
  let saved = await accounts()
  expect(saved).toHaveLength(1)
  expect(JSON.stringify(await workspace.api('/api/plugins'))).not.toContain('ops_browser_fixture')
  expect(saved[0].agentIds).toHaveLength(1)
  expect(saved[0].secretKeys).toEqual(['token'])

  await workspace.restart()
  await page.reload()
  const account = page.getByRole('article', { name: 'Production secrets' })
  await expect(account.getByText('Saved', { exact: true })).toBeVisible()
  await expect(account.getByText('Authorized agents: Main agent')).toBeVisible()
  await expect(account.getByRole('button', { name: 'Show vaults' })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('onepassword-instance.png'), fullPage: true, animations: 'disabled' })

  await account.getByRole('button', { name: 'Edit Production secrets', exact: true }).click()
  dialog = page.getByRole('dialog', { name: 'Edit Production secrets' })
  await expect(dialog.getByLabel('Service account token', { exact: true })).toHaveValue('')
  await expect(dialog.getByRole('checkbox', { name: 'Main agent', exact: true })).toBeChecked()
  await dialog.getByRole('checkbox', { name: 'Main agent', exact: true }).uncheck()
  await dialog.getByRole('checkbox', { name: 'Active', exact: true }).uncheck()
  await dialog.getByRole('button', { name: 'Save service account' }).click()
  await expect(account.getByText('No agent can use it yet.')).toBeVisible()
  await expect(account.getByRole('status').filter({ hasText: 'Paused' })).toBeVisible()
  saved = await accounts()
  expect(saved[0].agentIds).toEqual([])
  expect(saved[0].enabled).toBe(false)
  // The saved token survived an edit that left it blank.
  expect(saved[0].secretKeys).toEqual(['token'])

  await account.getByRole('button', { name: 'Delete Production secrets', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete service account', exact: true }).click()
  await expect(page.getByText('No service accounts yet. Add one to start using this plugin.')).toBeVisible()
})
