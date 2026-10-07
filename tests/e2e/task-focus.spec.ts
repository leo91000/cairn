import { expect, expectSingleScroll, test } from './fixtures'

test('reconnects after a temporary restart and resumes a cancelled conversation in place', async ({ page, workspace }) => {
  const agent = workspace.service.agent({ name: 'Recovery engineer' })
  const project = workspace.service.store.list('projects')[0]
  const task = workspace.service.task({
    name: 'Recover a deployment review',
    agentId: agent.id,
    projectId: project.id,
    prompt: 'fixture:restart',
    worktree: false,
  })
  const run = await workspace.service.enqueue(task.id)
  await expect.poll(() => workspace.service.store.run(run.id)?.sessionId).toBe('fixture-session')
  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  let available = false
  await page.route(`**/api/runs/${run.id}/stream?*`, async (route) => {
    if (available)
      await route.continue()
    else
      await route.fulfill({ status: 503, json: { error: 'Worker is restarting' } })
  })
  await page.goto(`/runs/${run.id}`)
  await expect(page.getByRole('status').filter({ hasText: 'Reconnecting' })).toBeVisible()
  available = true
  await expect(page.getByRole('heading', { name: task.name })).toBeVisible()
  await expect(page.getByRole('status').filter({ hasText: 'Reconnecting' })).toHaveCount(0)
  await page.getByRole('button', { name: 'Stop run', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Stop run', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expectSingleScroll(page)
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('resume-mobile-dark.png') })
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeInViewport()
  // The worker can finish before the resume HTTP response reaches the browser.
  // A late response must preserve a view selected while that request was pending.
  let releaseResume!: () => void
  const resumeResponse = new Promise<void>((resolve) => {
    releaseResume = resolve
  })
  await page.route(`**/api/runs/${run.id}/resume`, async (route) => {
    const response = await route.fetch()
    await resumeResponse
    await route.fulfill({ response })
  })
  await page.getByRole('button', { name: 'Resume', exact: true }).click()
  await expect.poll(() => workspace.service.store.run(run.id)?.status).toBe('succeeded')
  await page.getByRole('button', { name: 'Result', exact: true }).click()
  releaseResume()
  await expect(page.getByText('Resuming saved conversation', { exact: true })).toBeVisible()
  await expect(page.getByText('The saved conversation and work survived the restart.', { exact: true })).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('resumed-desktop-light.png') })
  expect(workspace.service.store.runs().filter(item => item.taskId === task.id)).toHaveLength(1)
})

test('keeps the selected mission across reloads and runs it from its mobile sheet', async ({ page, workspace }) => {
  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  const agent = workspace.service.store.list('agents')[0]
  const task = workspace.service.task({
    name: 'Fresh task without a run',
    prompt: 'This is the new task brief. fixture:hang',
    agentId: agent.id,
    enabled: false,
    worktree: false,
  })
  await page.reload()
  await page.getByRole('button', { name: 'Open Fresh task without a run', exact: true }).click()
  const detail = page.getByTestId('mission-detail')
  await expect(detail).toContainText('This is the new task brief.')
  await expect(detail).toContainText('No runs yet.')
  await expect(page).toHaveURL(new RegExp(`task=${task.id}`))
  await page.reload()
  await expect(page.getByRole('heading', { name: 'Fresh task without a run', level: 2 })).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  const sheet = page.getByRole('dialog', { name: 'Fresh task without a run' })
  await expect(sheet).toBeVisible()
  await sheet.getByRole('button', { name: 'Close mission', exact: true }).click()
  await expect(sheet).toHaveCount(0)
  await page.getByRole('button', { name: 'Open Fresh task without a run', exact: true }).click()
  await page.setViewportSize({ width: 390, height: 664 })
  await sheet.getByRole('button', { name: 'Run now', exact: true }).click()
  await expect(page).toHaveURL(/\/runs\/[\w-]+$/)
  await expect(page.getByRole('heading', { name: task.name })).toBeVisible()
  await page.getByRole('button', { name: 'Stop run', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Stop run', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect.poll(() => workspace.service.store.runs().find(item => item.taskId === task.id)?.status).toBe('cancelled')
})

test('mission cards and sheet show the schedule, the brief and the history, as on Android', async ({ page, workspace }) => {
  await workspace.restart()
  const agent = workspace.service.store.list('agents')[0]
  const project = workspace.service.store.list('projects')[0]
  const task = await workspace.api('/api/tasks', 'POST', {
    name: 'Weekly Graphile Worker upstream ports',
    agentId: agent.id,
    projectId: project.id,
    prompt: 'Review upstream changes and report the checks you ran. fixture:activity',
    cron: '0 9 * * 4',
    enabled: false,
    worktree: false,
  })
  const run = await workspace.service.enqueue(task.id)
  await expect.poll(() => workspace.service.store.run(run.id)?.status).toBe('succeeded')
  const reply = '## The upstream review is complete.\n\nThe applicable changes have been integrated. No identified change remains pending.'
  workspace.service.store.event(run.id, 'item.completed', reply, { item: { type: 'agent_message', text: reply } })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Missions', exact: true })).toBeVisible()
  const card = page.locator('.mission-card').filter({ hasText: task.name })
  await expect(card).toContainText('Paused · Every Thursday · 09:00')
  await expect(card.getByLabel('1 of the last 1 runs succeeded')).toBeVisible()
  await expectSingleScroll(page)
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('missions-mobile-dark.png') })
  await page.getByRole('button', { name: 'Search missions', exact: true }).click()
  await page.getByRole('textbox', { name: 'Search missions' }).fill('no matching task')
  await expect(page.getByRole('heading', { name: 'No matching missions' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Search missions' }).fill('Graphile')
  await page.getByRole('button', { name: `Open ${task.name}`, exact: true }).click()
  const sheet = page.getByRole('dialog', { name: task.name })
  await expect(sheet.getByRole('region', { name: 'Schedule' })).toContainText('0 9 * * 4 · Europe/Paris')
  await expect(sheet.getByRole('region', { name: 'Schedule' })).toContainText('09:00')
  await expect(sheet).toContainText('100% success')
  await sheet.getByRole('button', { name: 'Show the whole mission' }).click()
  await expect(sheet.getByRole('region', { name: 'Mission brief' })).toContainText('Review upstream changes')
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('mission-sheet-mobile-dark.png') })
  await sheet.getByRole('list', { name: 'Run history' }).getByRole('link').first().click()
  await expect(page).toHaveURL(`/runs/${run.id}`)
  await expect(page.getByRole('heading', { name: 'The upstream review is complete.' })).toBeVisible()
  await page.goBack()
  await page.setViewportSize({ width: 1440, height: 960 })
  await page.emulateMedia({ colorScheme: 'light' })
  await expect(page.getByRole('region', { name: 'Selected mission' }).getByRole('heading', { name: task.name })).toBeVisible()
  await expectSingleScroll(page)
  await page.screenshot({ animations: 'disabled', path: test.info().outputPath('missions-desktop.png') })
  for (const status of ['needs_input', 'blocked'] as const) {
    workspace.service.store.updateRun(run.id, {
      outcome: {
        status,
        reason: 'Review the upstream API change before continuing.',
        evidence: [],
        reportedAt: Date.now(),
      },
    })
    await page.reload()
    await expect(page.locator('.mission-card').filter({ hasText: task.name })).toContainText(status === 'needs_input' ? 'Your input needed' : 'Blocked')
  }

  await workspace.restart()
})
