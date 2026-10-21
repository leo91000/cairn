import type { Page, TestInfo } from '@playwright/test'
import type { Workspace } from './fixtures'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

// A failed mission needs the user; a hanging conversation keeps an agent working.
async function seed(workspace: Workspace) {
  const agent = workspace.service.agent({ name: 'Ops' })
  const task = workspace.service.task({
    name: 'Nightly security audit',
    prompt: 'Audit dependencies. fixture:fail',
    agentId: agent.id,
    projectId: null,
    skills: null,
    worktree: false,
    enabled: false,
    cron: null,
  })
  const run = await workspace.service.enqueue(task.id)
  await expect.poll(() => workspace.service.store.run(run.id)?.status, { timeout: 30000 }).toBe('failed')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep watching the build. fixture:chat-hang' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('running')
  return { chat, run }
}

async function signIn(page: Page) {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Fil', exact: true }).first()).toBeVisible({ timeout: 30000 })
}

async function capture(page: Page, testInfo: TestInfo, name: string) {
  await page.evaluate(() => document.fonts.ready)
  await page.screenshot({ path: testInfo.outputPath(`${name}.png`) })
}

test('exclusive suspension stays live and suppresses working indicators until release', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep watching the build. fixture:chat-hang' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('running')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const runId = detail.run.id
  workspace.service.store.updateRun(runId, {
    suspendedByExclusive: {
      id: randomUUID(),
      nodeId: randomUUID(),
      ownerRunId: randomUUID(),
      reason: 'Compile the emulator image',
      state: 'acquired',
      createdAt: Date.now(),
      expiresAt: Date.now() + 60000,
      waitUntil: Date.now() + 60000,
      pausedRunIds: [runId],
      minimumAvailableMemoryMiB: 0,
    },
  })
  try {
    await signIn(page)
    const live = page.getByRole('region', { name: 'Live', exact: true })
    await expect(live.getByText('Suspended for exclusive work', { exact: true })).toBeVisible()
    await expect(live.getByText('Working', { exact: true })).toHaveCount(0)
    await expect(live.locator('.avatar-orbit')).toHaveCount(0)
    await live.locator(`[data-fil-key="chat:${chat.id}"]`).click()
    await expect(page.getByRole('region', { name: 'Activity output' }).getByText('Suspended for exclusive work.')).toBeVisible()
    await expect(page.locator('.agent-working, .working-wave')).toHaveCount(0)
    workspace.service.store.updateRun(runId, { suspendedByExclusive: null })
    await expect(live.getByText('Working', { exact: true })).toBeVisible()
    await expect(page.locator('.agent-working')).toHaveCount(1)
  }
  finally {
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
    await workspace.api(`/api/chats/${chat.id}`, 'DELETE', { confirm: true })
  }
})

test('the Fil, rail and shortcuts lead through the workspace', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  const { chat, run } = await seed(workspace)
  await signIn(page)

  const forYou = page.getByRole('region', { name: 'For you', exact: true })
  await expect(forYou.getByText('Nightly security audit', { exact: true })).toBeVisible()
  await expect(forYou.getByText('Mission failed', { exact: true })).toBeVisible()
  await expect(forYou.getByRole('button', { name: 'Run “Nightly security audit” again' })).toBeVisible()
  const live = page.getByRole('region', { name: 'Live', exact: true })
  await expect(live.getByText('Working', { exact: true })).toBeVisible()
  await expect(live.locator('.avatar-orbit')).toHaveCount(1)
  const rail = page.getByRole('navigation', { name: 'Workspace navigation' })
  const fil = rail.getByRole('link', { name: 'Fil', exact: true })
  await expect(fil).toHaveAttribute('aria-current', 'page')
  await expect(fil).toHaveAttribute('aria-description', '1 need you')
  await expectSingleScroll(page)
  await capture(page, testInfo, 'signal-fil-desktop')

  // J selects the first item and Enter opens it, like a mail client. A mission reads beside the
  // Fil, which stays the current place, and its Mission tab holds the mission itself.
  await page.keyboard.press('j')
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(workspacePath(`/fil/runs/${run.id}`))
  await expect(fil).toHaveAttribute('aria-current', 'page')
  await expect(forYou.locator('[aria-current="page"]')).toHaveCount(0)
  await expect(forYou.locator('article.border-accent')).toContainText('Nightly security audit')
  const pane = page.getByRole('region', { name: 'Reading pane' })
  await expect(pane.getByRole('heading', { name: 'Nightly security audit' })).toBeVisible()
  await pane.getByRole('button', { name: 'Mission', exact: true }).click()
  await expect(pane.getByTestId('mission-detail').getByRole('region', { name: 'Schedule' })).toBeVisible()
  await expect(pane.getByRole('heading', { name: 'This run' })).toBeVisible()
  await expectSingleScroll(page)
  await capture(page, testInfo, 'signal-fil-mission-desktop')

  // G chords move between the three places.
  await page.keyboard.press('g')
  await page.keyboard.press('m')
  await expect(page.getByRole('heading', { name: 'Missions' })).toBeVisible()
  await page.keyboard.press('g')
  await page.keyboard.press('a')
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await expect(page.getByRole('navigation', { name: 'Atelier sections' }).getByRole('link', { name: 'Atelier overview' })).toHaveAttribute('aria-current', 'page')
  await expect(page.getByRole('region', { name: 'Resources' }).getByRole('link', { name: 'Agents', exact: true })).toBeVisible()
  await capture(page, testInfo, 'signal-atelier-desktop')

  await page.keyboard.press('Shift+Slash')
  const shortcuts = page.getByRole('dialog', { name: 'Keyboard shortcuts' })
  await expect(shortcuts.getByText('Search and run commands', { exact: true })).toBeVisible()
  await capture(page, testInfo, 'signal-shortcuts-desktop')
  await page.keyboard.press('Escape')
  await expect(shortcuts).toHaveCount(0)

  // The palette finds conversations and missions and acts on them directly.
  await page.keyboard.press('Control+k')
  const palette = page.getByRole('dialog', { name: 'Search and commands' })
  await expect(palette).toBeVisible()
  await page.keyboard.type('nightly')
  // What needs the user ranks first; missions can be run from here.
  await expect(palette.getByRole('option').first()).toContainText('Nightly security audit')
  await capture(page, testInfo, 'signal-palette-desktop')
  await palette.getByRole('option', { name: /Run “Nightly security audit” now/ }).click()
  await expect(palette).toHaveCount(0)
  await expect(page.getByRole('status').filter({ hasText: 'Nightly security audit started' })).toBeVisible()
  await expect.poll(async () => (await workspace.api('/api/runs')).filter((item: { taskId: string }) => item.taskId === run.taskId).length).toBe(2)

  // The conversation keeps the Fil beside it and names the agent's live step.
  await page.keyboard.press('g')
  await page.keyboard.press('f')
  await live.getByRole('button', { name: /Keep watching the build/ }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${chat.slug}`))
  await expect(page.getByRole('complementary', { name: 'Fil' }).locator('[aria-current="page"]')).toBeVisible()
  const working = page.locator('.agent-working')
  await expect(working).toBeVisible()
  await expect(working).toHaveAttribute('aria-live', 'polite')
  await expect(page.locator('.chat-page .avatar-orbit')).toHaveCount(1)
  await expectSingleScroll(page)
  await capture(page, testInfo, 'signal-conversation-desktop')

  // R focuses the reply field, Escape leaves it and C starts a new conversation.
  await page.keyboard.press('r')
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeFocused()
  await page.keyboard.press('Escape')
  await page.keyboard.press('c')
  await expect(page).toHaveURL(workspacePath('/chats'))
  await expect(page.getByRole('heading', { name: 'What are we building?' })).toBeVisible()
  // Free the runner for the next journey in this worker.
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})

test('phones use the floating dock and read conversations full screen', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  const { chat, run } = await seed(workspace)
  await page.setViewportSize({ width: 390, height: 844 })
  await signIn(page)
  const dock = page.getByRole('navigation', { name: 'Quick navigation' })
  await expect(dock).toBeVisible()
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expect(page.getByRole('region', { name: 'For you', exact: true }).getByText('Nightly security audit', { exact: true }).first()).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
  await expectSingleScroll(page)
  await capture(page, testInfo, 'signal-fil-mobile')

  await dock.getByRole('link', { name: 'Missions' }).click()
  await expect(page.getByRole('heading', { name: 'Missions' })).toBeVisible()
  await dock.getByRole('link', { name: 'Atelier' }).click()
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible()
  await capture(page, testInfo, 'signal-atelier-mobile')
  await dock.getByRole('link', { name: 'Fil' }).click()

  await page.getByRole('region', { name: 'Live', exact: true }).getByRole('button').first().click()
  await expect(page).toHaveURL(workspacePath(`/chats/${chat.slug}`))
  await expect(dock).toHaveCount(0)
  await expect(page.locator('.agent-working')).toBeVisible()
  await capture(page, testInfo, 'signal-conversation-mobile')
  await page.getByRole('link', { name: 'Back to the Fil' }).click()
  await expect(page).toHaveURL(workspacePath('/'))
  await expect(dock).toBeVisible()

  // A mission opens full screen from the Fil and returns to it.
  // Earlier journeys in this worker seeded the same mission name; open this journey's own card.
  await page.locator(`[data-fil-key="task:${run.taskId}"]`).getByRole('button', { name: 'Open “Nightly security audit”' }).click()
  await expect(page).toHaveURL(workspacePath(`/fil/runs/${run.id}`))
  await expect(dock).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Nightly security audit' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'Fil', exact: true })).toBeHidden()
  await capture(page, testInfo, 'signal-fil-mission-mobile')
  await page.getByRole('link', { name: 'Back to the Fil' }).click()
  await expect(page).toHaveURL(workspacePath('/'))
  await expect(dock).toBeVisible()
  // Free the runner for the next journey in this worker.
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})

// Blocked missions: one waits on the user, one on the conversation that started it, one on an
// outside check, and one needs input that can only be snoozed.
async function blockedMission(workspace: Workspace, agentId: string, name: string, outcome: Record<string, unknown>, requestedBy?: Record<string, unknown>) {
  const task = workspace.service.task({
    name,
    prompt: 'Prove it on a device.',
    agentId,
    projectId: null,
    skills: null,
    worktree: false,
    enabled: false,
    cron: null,
  })
  const run = await workspace.service.enqueue(task.id)
  await expect.poll(() => workspace.service.store.run(run.id)?.status, { timeout: 30000 }).toBe('succeeded')
  workspace.service.store.updateRun(run.id, {
    outcome: { evidence: ['RetryNoticeDeviceTest 2/2 on API 36 AOSP'], reportedAt: Date.now(), ...outcome },
    ...(requestedBy ? { requestedBy } : {}),
  } as any)
  return { task, run }
}

test('blocked missions ask only when they wait on the user, and can be set aside', async ({ page, workspace }, testInfo) => {
  test.setTimeout(180000)
  await useRelayForHttpMocks(page)
  const { id: agent } = workspace.service.agent({ name: 'Claude' })
  const parent = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...parent, title: 'Orchestrate the F8 architecture work' })
  const proof = await blockedMission(workspace, agent, 'F8 · device proof c9f939e5', {
    status: 'blocked',
    reason: 'The device proof passed; delivery waits for the CI run and the merge.',
  })
  const scope = await blockedMission(workspace, agent, 'Release notes for 0.53', {
    status: 'needs_input',
    reason: 'Choose whether the notes cover the Android release.',
  })
  const handedBack = await blockedMission(workspace, agent, 'F8 · PR315 checks', {
    status: 'blocked',
    reason: 'The parent merges once CI is green, as it asked.',
    waitingOn: 'requester',
  }, { runId: randomUUID(), chatId: parent.id })
  const external = await blockedMission(workspace, agent, 'Nightly staging deploy', {
    status: 'blocked',
    reason: 'The staging deploy waits for the cloud provider to finish its maintenance.',
    waitingOn: 'external',
  })
  await page.emulateMedia({ colorScheme: 'dark', reducedMotion: 'reduce' })
  await page.setViewportSize({ width: 1440, height: 900 })
  await signIn(page)

  const forYou = page.getByRole('region', { name: 'For you', exact: true })
  const recent = page.getByRole('region', { name: 'Recent', exact: true })
  const card = (task: { id: string }) => forYou.locator(`[data-fil-key="task:${task.id}"]`)
  await expect(card(proof.task)).toContainText('Blocked · The device proof passed')
  await expect(card(proof.task).getByRole('button', { name: `Dismiss “${proof.task.name}”` })).toBeVisible()
  // Input the agent asked for can only be snoozed.
  await expect(card(scope.task)).toBeVisible()
  await expect(card(scope.task).getByRole('button', { name: /^Dismiss/ })).toHaveCount(0)
  // What waits on the conversation that started it, or on an outside check, never asks the user.
  await expect(forYou.locator(`[data-fil-key="task:${handedBack.task.id}"], [data-fil-key="task:${external.task.id}"]`)).toHaveCount(0)
  await expect(recent.locator(`[data-fil-key="task:${handedBack.task.id}"]`)).toContainText('Waiting on Orchestrate the F8 architecture work')
  await expect(recent.locator(`[data-fil-key="task:${external.task.id}"]`)).toContainText('Waiting on an outside check')
  await capture(page, testInfo, 'blocked-fil-for-you')

  // A refused write must roll back even when the following activity read is unavailable.
  const dismissalsRoute = '**/api/dismissals'
  const activityRoute = '**/api/tasks/activity'
  await page.route(dismissalsRoute, route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Dismissal unavailable' }) }))
  await page.route(activityRoute, route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Activity unavailable' }) }))
  const refusedDismissal = page.waitForResponse(response => response.url().endsWith('/api/dismissals') && response.status() === 503)
  await card(proof.task).getByRole('button', { name: `Dismiss “${proof.task.name}”` }).click()
  await refusedDismissal
  await expect(card(proof.task)).toBeVisible()
  await page.unroute(dismissalsRoute)
  await page.unroute(activityRoute)

  await card(scope.task).getByRole('button', { name: `More actions for “${scope.task.name}”` }).click()
  const menu = page.getByRole('menu')
  await expect(menu.getByRole('menuitem')).toHaveText(['Remind me tomorrow', 'Run again', 'Archive mission'])
  await capture(page, testInfo, 'blocked-fil-menu')
  await menu.getByRole('menuitem', { name: 'Remind me tomorrow' }).click()
  const waiting = page.getByRole('region', { name: 'Waiting', exact: true })
  await expect(waiting.locator(`[data-fil-key="task:${scope.task.id}"]`)).toContainText('Snoozed')

  // The run page says what the account can do, under the outcome itself.
  await card(proof.task).getByRole('button', { name: `Open “${proof.task.name}”` }).click()
  const actions = page.getByTestId('mission-outcome-actions')
  await expect(actions.getByRole('button', { name: 'Remind me tomorrow' })).toBeVisible()
  await capture(page, testInfo, 'blocked-run-actions')

  await actions.getByRole('button', { name: 'Dismiss' }).click()
  await expect(forYou.locator(`[data-fil-key="task:${proof.task.id}"]`)).toHaveCount(0)
  await expect(recent.locator(`[data-fil-key="task:${proof.task.id}"]`)).toContainText('Dismissed')
  await expect(actions).toContainText('Dismissed: it stays out of For you until the mission reports again.')
  await capture(page, testInfo, 'blocked-run-dismissed')

  // Set aside for this account, on every device: it stays so after a reload.
  await page.reload()
  await expect(recent.locator(`[data-fil-key="task:${proof.task.id}"]`)).toContainText('Dismissed')
  await expect(waiting.locator(`[data-fil-key="task:${scope.task.id}"]`)).toContainText('Snoozed')

  await recent.locator(`[data-fil-key="task:${handedBack.task.id}"]`).click()
  await expect(actions).toContainText('Waiting on Orchestrate the F8 architecture work, which started this mission.')
  await capture(page, testInfo, 'blocked-run-waiting-on-requester')
  await actions.getByRole('link', { name: 'Orchestrate the F8 architecture work' }).click()
  await expect(page).toHaveURL(/\/chats\//)

  // The Fil once both have been set aside.
  await page.goto(workspacePath('/'))
  await expect(waiting.locator(`[data-fil-key="task:${scope.task.id}"]`)).toBeVisible()
  await capture(page, testInfo, 'blocked-fil-set-aside')

  await waiting.locator(`[data-fil-key="task:${scope.task.id}"]`).click()
  await actions.getByRole('button', { name: 'Back to For you' }).click()
  await expect(card(scope.task)).toBeVisible()

  // The keyboard dismisses a blocked mission as it reads finished work.
  await page.goto(workspacePath(`/fil/runs/${proof.run.id}`))
  await actions.getByRole('button', { name: 'Back to For you' }).click()
  await expect(card(proof.task)).toBeVisible()
  await page.goto(workspacePath('/'))
  await expect(card(proof.task)).toBeVisible()
  // K climbs to the top of the Fil; what needs an answer comes newest first, then the device proof.
  for (let step = 0; step < 6; step++)
    await page.keyboard.press('k')
  await page.keyboard.press('j')
  await expect(forYou.locator('article.border-accent')).toContainText(proof.task.name)
  await page.keyboard.press('e')
  await expect(recent.locator(`[data-fil-key="task:${proof.task.id}"]`)).toContainText('Dismissed')
})
