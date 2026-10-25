import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  expectUncovered,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('a completed mission becomes a chat and follows up in the same session after reload', async ({ page, workspace }) => {
  const project = workspace.service.store.list('projects')[0]
  const task = workspace.service.task({
    name: 'Deployment review to discuss',
    agentId: workspace.service.store.list('agents')[0]!.id,
    projectId: project.id,
    prompt: 'Review this deployment before release.',
    worktree: false,
  })
  const run = await workspace.service.enqueue(task.id)
  await expect.poll(() => workspace.service.store.run(run.id)?.status).toBe('succeeded')
  const original = workspace.service.store.run(run.id)!
  await page.goto(workspacePath(`/runs/${run.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Continue in chat', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[^/]+$/)
  const chats = await workspace.api('/api/chats')
  const chat = chats.find((chat: { runId: string }) => chat.runId === run.id)
  expect(chat).toBeDefined()
  await page.reload()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(detail.run.sessionId).toBe(original.sessionId)
  expect(detail.run.workspace).toBe(original.workspace)
  expect(detail.run.summary).toBe(original.summary)
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('What should I monitor after this deployment?')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => {
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    return detail.messages.length === 0 && detail.run.status === 'succeeded' && detail.run.finishedAt > original.finishedAt!
  }).toBe(true)
  const followed = await workspace.api(`/api/chats/${chat.id}`)
  expect(followed.run.id).toBe(run.id)
  expect(followed.run.sessionId).toBe(original.sessionId)
  expect(followed.run.workspace).toBe(original.workspace)
  expect(followed.run.snapshot.task.prompt).toBe('What should I monitor after this deployment?')
  await expect(page.getByText('Ready for the next step.', { exact: false })).toBeVisible()
  await expect(page.getByText('The fixture task passed.', { exact: true })).toBeVisible()
})

test('reconnects after a temporary restart and resumes a cancelled conversation in place', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
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
  await expect.poll(() => workspace.service.store.run(run.id)?.sessionId).toBe('fixture-chat')
  await page.goto(workspacePath('/tasks'))
  await authenticateWorkspace(page)
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  let available = false
  await page.route(`**/api/runs/${run.id}/stream?*`, async (route) => {
    if (available)
      await route.continue()
    else
      await route.fulfill({ status: 503, json: { error: 'Worker is restarting' } })
  })
  await page.goto(workspacePath(`/runs/${run.id}`))
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

for (const failure of ['refresh', 'lost response'] as const) {
  test(`mission retry preserves one scheduled mission after ${failure}`, async ({ page, workspace }, testInfo) => {
    await useRelayForHttpMocks(page)
    await page.goto(workspacePath('/tasks'))
    await authenticateWorkspace(page)
    await page.getByRole('button', { name: 'New mission', exact: true }).click()
    const dialog = page.getByRole('dialog')
    const name = `Scheduled retry ${failure}`
    await dialog.getByLabel('Mission name', { exact: true }).fill(name)
    await dialog.getByLabel('What should happen?').fill('Keep exactly one scheduled mission.')
    await dialog.getByRole('combobox', { name: 'When', exact: true }).click()
    await page.getByRole('option', { name: 'Every Monday', exact: true }).click()
    const creations: string[] = []
    let rejectRefresh = false
    let createdId = ''
    let updates = 0
    await page.route('**/api/tasks/*', async (route) => {
      if (route.request().method() === 'PUT')
        updates++
      await route.continue()
    })
    await page.route('**/api/tasks', async (route) => {
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
        await route.fulfill({ status: 400, json: { error: 'Mission list temporarily rejected' } })
      }
      else {
        await route.continue()
      }
    })

    await page.setViewportSize({ width: 390, height: 844 })
    await dialog.getByRole('button', { name: 'Create mission', exact: true }).click()
    await expect(dialog.getByRole('alert')).toContainText(failure === 'refresh' ? 'Mission saved, but the workspace could not be reloaded' : 'Could not confirm whether the mission was saved')
    await expectUncovered(dialog.getByRole('alert'))
    rejectRefresh = false
    const created = (await workspace.api('/api/tasks')).find((task: { id: string }) => task.id === createdId)
    expect(created.cron).toBe('0 9 * * 1')
    await page.screenshot({ path: testInfo.outputPath(`mission-${failure.replaceAll(' ', '-')}-mobile.png`), animations: 'disabled' })
    await dialog.getByLabel('Mission name', { exact: true }).fill(`${name} edited`)
    await dialog.getByRole('button', { name: failure === 'refresh' ? 'Save changes' : 'Create mission', exact: true }).click()
    await expect(page.getByRole('dialog', { name: /Give your agent a mission|Edit mission/ })).toHaveCount(0)

    const saved = (await workspace.api('/api/tasks')).filter((task: { name: string }) => task.name.startsWith(name))
    expect(saved).toHaveLength(1)
    expect(saved[0]).toMatchObject({
      id: createdId,
      name: `${name} edited`,
      createdAt: created.createdAt,
      cron: '0 9 * * 1',
    })
    expect(updates).toBe(1)
    expect(creations).toHaveLength(failure === 'refresh' ? 1 : 2)
    if (failure === 'lost response')
      expect(creations[1]).toBe(creations[0])
  })
}

test('keeps the selected mission across reloads and runs it from its mobile sheet', async ({ page, workspace }) => {
  await page.goto(workspacePath('/tasks'))
  await authenticateWorkspace(page)
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
  await page.goto(workspacePath('/tasks'))
  await authenticateWorkspace(page)
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
  await expect(page).toHaveURL(workspacePath(`/runs/${run.id}`))
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
