import { execFileSync } from 'node:child_process'
import { mkdir } from 'node:fs/promises'
import path from 'node:path'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  initializeRepository,
  test,
  workspacePath,
} from './fixtures'

test('reviews the open workspace layout and compact chat controls across themes and sizes', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  initializeRepository(workspace.projectPath)
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()

  for (const theme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme: theme })
    for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }]) {
      await page.setViewportSize(viewport)
      await expectSingleScroll(page)
      await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeInViewport()
      await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`chat-new-${theme}-${viewport.width}.png`) })
    }
  }

  // The inline selector opens above the composer and keeps keyboard selection.
  const project = page.getByRole('combobox', { name: 'Chat project' })
  await project.click()
  await expect(page.getByRole('listbox', { name: 'Chat project' })).toBeInViewport({ ratio: 1 })
  await project.fill('Design')
  await project.press('ArrowDown')
  await project.press('Enter')
  await expect(project).toHaveValue('Design system')
  await expectSingleScroll(page)
  await page.getByRole('button', { name: 'Conversations', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Conversations' })).toBeVisible()
  await page.getByRole('button', { name: 'Close conversations' }).click()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()

  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Review our component architecture. fixture:chat-hang')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.locator('.activity-working')).toBeVisible()
  await expect(page.locator('.activity-message').filter({ hasText: 'component boundaries' })).toBeVisible()
  // The follow control lives in the desktop activity toolbar.
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.getByRole('button', { name: 'Follow output', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Follow output', exact: true })).toHaveAttribute('aria-pressed', 'false')
  for (const viewport of [{ width: 1440, height: 1000 }, { width: 390, height: 844 }, { width: 320, height: 600 }]) {
    await page.setViewportSize(viewport)
    await page.emulateMedia({ colorScheme: 'dark' })
    await expectSingleScroll(page)
    await expect(page.getByRole('button', { name: 'Stop response' })).toBeInViewport()
    await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`chat-conversation-dark-${viewport.width}.png`) })
  }

  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.getByRole('button', { name: 'Conversations', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Conversations' })).toBeVisible()
  await expect(page.locator('.activity-scroll')).toBeVisible()
  await expectSingleScroll(page)
  await page.screenshot({ animations: 'disabled', path: testInfo.outputPath('chat-history-dark.png') })
  await page.getByRole('button', { name: 'Close conversations' }).click()
  await page.getByRole('button', { name: 'Stop response' }).click()

  for (const route of ['/tasks', '/agents', '/projects', '/skills', '/runs', '/mcps', '/connections', '/settings']) {
    await page.goto(workspacePath(route))
    await expect(page.locator('.page h1').first()).toBeVisible()
    for (const theme of ['dark', 'light'] as const) {
      await page.emulateMedia({ colorScheme: theme })
      await page.setViewportSize({ width: 1440, height: 1000 })
      await expectSingleScroll(page)
      await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`${route.slice(1)}-${theme}.png`) })
    }

    await page.setViewportSize({ width: 390, height: 844 })
    await expectSingleScroll(page)
    await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`${route.slice(1)}-mobile.png`) })
  }

  expect(errors).toEqual([])
})

test('keeps projects compact and agent summaries readable with several resources', async ({ page, workspace }, testInfo) => {
  const projects = []
  for (const [name, directory] of [['Web app', 'web-app'], ['API', 'api'], ['Documentation and developer guides', 'documentation']]) {
    const projectPath = path.join(workspace.projectPath, directory!)
    await mkdir(projectPath, { recursive: true })
    initializeRepository(projectPath)
    execFileSync('git', ['-C', projectPath, 'remote', 'add', 'origin', `https://github.com/example/${directory}.git`])
    projects.push(await workspace.service.project({ name, path: projectPath, baseBranch: directory === 'documentation' ? 'docs/developer-guides' : 'main' }))
  }

  workspace.service.agent({ name: 'UI builder', description: 'Builds interfaces, checks accessibility and reviews the mobile experience.', access: { projects: [projects[0]!.id], github: false } })
  workspace.service.agent({ name: 'Code reviewer', description: 'Reviews changes across repositories and keeps regressions out.', access: { projects: projects.map(project => project.id), github: false } })
  await page.emulateMedia({ reducedMotion: 'reduce' })
  await page.goto(workspacePath('/projects'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Projects', exact: true })).toBeVisible()
  for (const resource of ['projects', 'agents']) {
    await page.goto(workspacePath(`/${resource}`))
    await expect(page.locator('.resource-card')).toHaveCount(4)
    for (const colorScheme of ['dark', 'light'] as const) {
      await page.emulateMedia({ colorScheme })
      for (const width of [1440, 390, 320]) {
        await page.setViewportSize({ width, height: width === 1440 ? 900 : 844 })
        await expectSingleScroll(page)
        if (width === 1440 && resource === 'projects') {
          const rows = await page.locator('.resource-card').evaluateAll(elements => elements.map(element => element.getBoundingClientRect().height))
          expect(rows.every(height => height < 110)).toBe(true)
        }

        await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`compact-${resource}-${colorScheme}-${width}.png`) })
      }
    }
  }

  await page.goto(workspacePath('/projects'))
  await page.getByRole('button', { name: 'Web app', exact: true }).click()
  await expect(page.getByRole('dialog').getByLabel('Project directory')).toHaveValue(projects[0]!.path)
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await page.locator('article').filter({ has: page.getByRole('heading', { name: 'Web app', exact: true }) }).getByRole('link', { name: 'Start chat' }).click()
  await expect(page.getByRole('combobox', { name: 'Chat project' })).toHaveValue('Web app')
})

test('reviews workflow permission, fresh project sources and explicit blocked outcomes', async ({ page, workspace }, testInfo) => {
  await page.emulateMedia({ reducedMotion: 'reduce' })
  // The server rate-limits each client; the previous journey spends this worker's budget.
  await workspace.restart()
  // The latest mission run; earlier journeys in this worker also leave conversation runs.
  const run = workspace.service.store.runs().filter(item => item.trigger !== 'chat').sort((a, b) => b.createdAt - a.createdAt)[0]!
  workspace.service.store.updateRun(run.id, {
    status: 'succeeded',
    summary: 'Validated locally. Delivery is blocked by the missing GitHub workflow permission.',
    outcome: {
      status: 'blocked',
      reason: 'The changes passed validation, but GitHub refused the workflow update.',
      evidence: ['42 tests passed', 'Push refused: missing workflow permission'],
      reportedAt: Date.now(),
    },
  })
  await page.route('**/api/connections?*', route => route.fulfill({
    json: [{
      provider: 'github',
      installed: true,
      connected: true,
      account: 'fixture',
      version: 'gh fixture',
      workflowPermission: false,
    }],
  }))
  await page.goto(workspacePath('/connections'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('button', { name: 'Enable workflow updates' })).toBeVisible()
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    await page.setViewportSize({ width: colorScheme === 'dark' ? 390 : 1440, height: 900 })
    await expectSingleScroll(page)
    await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`github-permission-${colorScheme}.png`) })
  }

  await page.goto(workspacePath('/projects'))
  await page.getByRole('button', { name: 'Design system', exact: true }).click()
  const mode = page.getByRole('combobox', { name: 'New work starts from' })
  await expect(mode).toHaveValue('Latest remote branch')
  await mode.click()
  await page.getByRole('option', { name: 'Local branch snapshot' }).click()
  await page.getByRole('button', { name: 'Save project' }).click()
  await expect(page.getByRole('dialog')).not.toBeVisible()
  await page.reload()
  await page.getByRole('button', { name: 'Design system', exact: true }).click()
  await expect(mode).toHaveValue('Local branch snapshot')
  await page.screenshot({ animations: 'disabled', path: testInfo.outputPath('project-source.png') })
  await page.getByRole('button', { name: 'Cancel', exact: true }).click()
  await page.goto(workspacePath(`/runs/${run.id}`))
  await expect(page.getByText('Execution finished', { exact: true })).toBeVisible()
  // The run header and the reply's outcome both say it; the header expands the evidence.
  await page.locator('.run-title-meta').getByText('Blocked', { exact: true }).click()
  await expect(page.getByText('Push refused: missing workflow permission', { exact: true }).first()).toBeVisible()
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    await page.setViewportSize({ width: colorScheme === 'dark' ? 390 : 1440, height: 900 })
    await expectSingleScroll(page)
    await page.screenshot({ animations: 'disabled', path: testInfo.outputPath(`blocked-outcome-${colorScheme}.png`) })
  }

  // The blocked mission says so on its card, as on Android.
  await page.goto(workspacePath('/tasks'))
  await expect(page.locator('.mission-card').filter({ hasText: run.snapshot.task.name })).toContainText('Blocked')
})
