import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  openAgentSection,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

const MAIN_AGENT_ID = '00000000-0000-4000-8000-000000000001'

test('1Password service accounts are plugin instances with private tokens, granted from the agent page', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  // The instance save is observed below, so requests must go through HTTP.
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/connections'))
  await authenticateWorkspace(page)
  // 1Password and GitHub live with the plugins only: Connections keeps coding agent accounts.
  await expect(page.getByRole('heading', { name: 'Coding agents' })).toBeVisible()
  await expect(page.getByText('1Password')).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'GitHub' })).toHaveCount(0)
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
  // Agents are granted on their own page; the instance only lists them.
  await expect(dialog.getByRole('checkbox', { name: 'Main agent', exact: true })).toHaveCount(0)
  await expect(dialog.getByRole('region', { name: 'Agents' })).toContainText('No agent can use it yet.')
  await expect(dialog.getByRole('link', { name: 'Manage on agent pages ›' })).toBeVisible()
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
  expect(saved[0].agentIds).toEqual([])
  expect(saved[0].secretKeys).toEqual(['token'])

  // Even the main agent gets an account only when granted on its page, after a review.
  await page.goto(workspacePath(`/agents/${MAIN_AGENT_ID}`))
  await openAgentSection(page, 'Secrets & plugins')
  const accounts1Password = page.getByRole('region', { name: '1Password · service accounts' })
  await expect(accounts1Password).toContainText('Read-only · no access by default, including Main')
  await expect(accounts1Password.getByText('Not granted', { exact: true })).toBeVisible()
  await accounts1Password.getByRole('switch', { name: 'Access to 1Password Production secrets' }).check()
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: 844 })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`onepassword-agent-${width}.png`), fullPage: true, animations: 'disabled' })
  }

  await page.setViewportSize({ width: 1440, height: 844 })

  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('Added: 1Password “Production secrets” — effective immediately, including in running conversations.')
  await review.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(review).toHaveCount(0)
  saved = await accounts()
  expect(saved[0].agentIds).toEqual([MAIN_AGENT_ID])
  expect((await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === MAIN_AGENT_ID).plugins).toEqual([saved[0].id])

  await workspace.restart()
  await page.goto(workspacePath('/plugins/onepassword'))
  const account = page.getByRole('article', { name: 'Production secrets' })
  await expect(account.getByText('Saved', { exact: true })).toBeVisible()
  await expect(account.getByText('Authorized agents: Main agent')).toBeVisible()
  await expect(account.getByRole('link', { name: 'Manage on agent pages ›' })).toBeVisible()
  await expect(account.getByRole('button', { name: 'Show vaults' })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('onepassword-instance.png'), fullPage: true, animations: 'disabled' })

  await account.getByRole('button', { name: 'Edit Production secrets', exact: true }).click()
  dialog = page.getByRole('dialog', { name: 'Edit Production secrets' })
  await expect(dialog.getByLabel('Service account token', { exact: true })).toHaveValue('')
  await expect(dialog.getByRole('region', { name: 'Agents' })).toContainText('Authorized agents: Main agent')
  const editing = page.waitForRequest(request => request.method() === 'PUT' && request.url().includes('/plugins/onepassword/connections/'))
  await dialog.getByRole('checkbox', { name: 'Active', exact: true }).uncheck()
  await dialog.getByRole('button', { name: 'Save service account' }).click()
  expect((await editing).postDataJSON()).not.toHaveProperty('agentIds')
  await expect(account.getByRole('status').filter({ hasText: 'Paused' })).toBeVisible()
  saved = await accounts()
  // Saving the instance keeps the grants made on agent pages.
  expect(saved[0].agentIds).toEqual([MAIN_AGENT_ID])
  expect(saved[0].enabled).toBe(false)
  // The saved token survived an edit that left it blank.
  expect(saved[0].secretKeys).toEqual(['token'])

  // The agent keeps the grant, shown as unusable while the account is disabled; revoking needs no review.
  await page.goto(workspacePath(`/agents/${MAIN_AGENT_ID}?section=plugins`))
  await expect(accounts1Password.getByText('Account disabled', { exact: true })).toBeVisible()
  await page.getByRole('link', { name: 'Overview', exact: true }).click()
  await expect(page.getByRole('status').filter({ hasText: '1 thing to check' })).toContainText('1Password “Production secrets” is disabled')
  await openAgentSection(page, 'Secrets & plugins')
  await accounts1Password.getByRole('switch', { name: 'Access to 1Password Production secrets' }).uncheck()
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('region', { name: 'Unsaved changes' })).toHaveCount(0)
  expect((await accounts())[0].agentIds).toEqual([])
  await page.goto(workspacePath('/plugins/onepassword'))
  await expect(account.getByText('No agent can use it yet.')).toBeVisible()

  await account.getByRole('button', { name: 'Delete Production secrets', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete service account', exact: true }).click()
  await expect(page.getByText('No service accounts yet. Add one to start using this plugin.')).toBeVisible()
})
