import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('agent access editor and task inheritance work on mobile in both themes', async ({ page }, testInfo) => {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible()
  await page.goto(workspacePath('/agents'))
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.getByRole('heading', { name: 'Main agent', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name', { exact: true }).fill('Scoped reviewer')
  await dialog.getByLabel('All projects, including future projects').uncheck()
  await dialog.getByLabel('Design system', { exact: true }).check()
  await dialog.getByLabel('All available skills').uncheck()
  await dialog.getByLabel('review', { exact: true }).check()
  await expect(dialog.getByLabel('Shared GitHub connection')).not.toBeChecked()
  await expect(dialog.getByLabel('Shared GitHub connection')).toBeDisabled()
  const mode = dialog.getByRole('combobox', { name: 'Execution mode', exact: true })
  await mode.click()
  await page.getByRole('option', { name: 'Read only', exact: true }).click()
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    await dialog.locator('.agent-access-panel').scrollIntoViewIfNeeded()
    await page.screenshot({ path: testInfo.outputPath(`agent-access-${colorScheme}.png`), animations: 'disabled' })
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true)
  }

  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await page.getByRole('link', { name: 'Missions', exact: true }).filter({ visible: true }).click()
  await page.getByRole('button', { name: 'New mission', exact: true }).click()
  await expect(page.getByRole('combobox', { name: 'Agent', exact: true })).toHaveValue('Main agent')
  await expect(page.getByRole('combobox', { name: 'Project context', exact: true })).toHaveCount(0)
  await page.getByRole('combobox', { name: 'Agent', exact: true }).click()
  await page.getByRole('option', { name: 'Scoped reviewer', exact: true }).click()
  await page.getByLabel('Mission name', { exact: true }).fill('Review the repository')
  await page.getByLabel('What should happen?').fill('Review code without making changes.')
  await expect(page.locator('.agent-scope-summary')).toContainText('1 allowed project')
  await page.screenshot({ path: testInfo.outputPath('agent-first-task.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Create mission', exact: true }).click()
  // As on Android, the new mission opens in its sheet on phones.
  await expect(page.getByRole('dialog', { name: 'Review the repository' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Review the repository' })).toBeVisible()
})

for (const kind of ['agents', 'projects'] as const) {
  for (const failure of ['refresh', 'lost response'] as const) {
    test(`${kind} retry updates the same resource after ${failure}`, async ({ page, workspace }, testInfo) => {
      await useRelayForHttpMocks(page)
      await page.goto(workspacePath(`/${kind}`))
      await authenticateWorkspace(page)
      const agent = kind === 'agents'
      const label = agent ? 'agent' : 'project'
      await page.getByRole('button', { name: agent ? 'New agent' : 'Add project', exact: true }).click()
      const dialog = page.getByRole('dialog')
      const name = `Retry ${kind} ${failure}`
      await dialog.getByLabel('Name', { exact: true }).fill(name)
      if (!agent)
        await dialog.getByLabel('Project directory').fill(workspace.projectPath)

      const creations: string[] = []
      let rejectRefresh = false
      let createdId = ''
      let updates = 0
      await page.route(`**/api/${kind}/*`, async (route) => {
        if (route.request().method() === 'PUT')
          updates++
        await route.continue()
      })
      await page.route(`**/api/${kind}`, async (route) => {
        const request = route.request()
        if (request.method() === 'POST') {
          creations.push(request.postData()!)
          const response = await route.fetch()
          createdId = (await response.json()).id
          if (creations.length === 1 && failure === 'lost response') {
            await route.abort('failed')
            return
          }

          rejectRefresh = creations.length === 1 && failure === 'refresh'
          await route.fulfill({ response })
        }
        else if (request.method() === 'GET' && rejectRefresh) {
          await route.fulfill({ status: 400, json: { error: 'Resource list temporarily rejected' } })
        }
        else {
          await route.continue()
        }
      })

      await dialog.getByRole('button', { name: `Save ${label}`, exact: true }).click()
      await expect(dialog.getByRole('alert')).toContainText(failure === 'refresh' ? 'saved, but the workspace could not be reloaded' : `Could not confirm whether the ${label} was saved`)
      rejectRefresh = false
      const first = (await workspace.api(`/api/${kind}`)).filter((item: { id: string }) => item.id === createdId)
      expect(first).toHaveLength(1)
      const createdAt = first[0].createdAt
      await page.setViewportSize({ width: 390, height: 844 })
      await page.screenshot({ path: testInfo.outputPath(`${kind}-${failure.replaceAll(' ', '-')}-mobile.png`), animations: 'disabled' })
      await dialog.getByLabel('Name', { exact: true }).fill(`${name} edited`)
      await dialog.getByRole('button', { name: `Save ${label}`, exact: true }).click()
      await expect(dialog).toHaveCount(0)

      const saved = (await workspace.api(`/api/${kind}`)).filter((item: { name: string }) => item.name.startsWith(name))
      expect(saved).toHaveLength(1)
      expect(saved[0]).toMatchObject({ id: createdId, name: `${name} edited`, createdAt })
      expect(updates).toBe(1)
      expect(creations).toHaveLength(failure === 'refresh' ? 1 : 2)
      if (failure === 'lost response')
        expect(creations[1]).toBe(creations[0])
    })
  }
}

test('a failed dedicated GitHub token keeps the created agent and retries only the connection and update', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/agents'))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name', { exact: true }).fill('Token retry agent')
  await dialog.getByLabel('Shared GitHub connection').uncheck()
  await dialog.getByLabel('Dedicated GitHub token').fill('fixture-token-not-a-secret')
  let tokenAttempts = 0
  let creates = 0
  page.on('request', (request) => {
    if (request.method() === 'POST' && request.url().endsWith('/api/agents'))
      creates++
  })
  await page.route('**/api/agents/*/github-token', async (route) => {
    tokenAttempts++
    await route.fulfill(tokenAttempts === 1
      ? { status: 400, json: { error: 'GitHub connection rejected' } }
      : { json: { configured: true } })
  })
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog.getByRole('alert')).toContainText('Agent saved, but the GitHub connection could not be saved')
  await expect(dialog.getByLabel('Dedicated GitHub token')).toHaveValue('fixture-token-not-a-secret')
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect(creates).toBe(1)
  expect(tokenAttempts).toBe(2)
  expect((await workspace.api('/api/agents')).filter((item: { name: string }) => item.name === 'Token retry agent')).toHaveLength(1)
})

test('agents start unlimited and retain an optional limit after saving', async ({ page, workspace }) => {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible({ timeout: 30000 })
  await page.goto(workspacePath('/agents'))
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  await dialog.getByLabel('Name', { exact: true }).fill('Long running agent')
  await expect(dialog.getByLabel('No time limit', { exact: true })).toBeChecked()
  await expect(dialog.getByLabel('Time limit (minutes)')).toHaveCount(0)
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  const saved = (await workspace.api('/api/agents')).find((agent: { name: string }) => agent.name === 'Long running agent')
  expect(saved.timeoutMinutes).toBe(0)
  await page.getByRole('button', { name: 'Edit Long running agent', exact: true }).click()
  await dialog.getByLabel('No time limit', { exact: true }).uncheck()
  await dialog.getByLabel('Time limit (minutes)').fill('45')
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await page.getByRole('button', { name: 'Edit Long running agent', exact: true }).click()
  await expect(dialog.getByLabel('Time limit (minutes)')).toHaveValue('45')
  await dialog.getByLabel('No time limit', { exact: true }).check()
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect((await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === saved.id).timeoutMinutes).toBe(0)
})

test('installation access is opt-in and can be enabled and revoked for any agent', async ({ page, workspace }) => {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.locator('.shell')).toBeVisible()
  await page.goto(workspacePath('/agents'))
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  const dialog = page.getByRole('dialog')
  const access = dialog.getByLabel('Allow access to the current Cairn installation', { exact: true })
  await dialog.getByLabel('Name', { exact: true }).fill('Installation helper')
  await expect(access).not.toBeChecked()
  await access.check()
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  const agent = (await workspace.api('/api/agents')).find((agent: { name: string }) => agent.name === 'Installation helper')
  expect(agent.access.installation).toBe(true)
  await page.getByRole('button', { name: 'Edit Installation helper', exact: true }).click()
  await expect(access).toBeChecked()
  await access.uncheck()
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect((await workspace.api('/api/agents')).find((item: { id: string }) => item.id === agent.id).access.installation).toBe(false)
  await page.getByRole('button', { name: 'Edit Main agent', exact: true }).click()
  await expect(access).not.toBeChecked()
  await access.check()
  await dialog.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  expect((await workspace.api('/api/agents')).find((item: { name: string }) => item.name === 'Main agent').access.installation).toBe(true)
})
