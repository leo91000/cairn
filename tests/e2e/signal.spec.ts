import type { Page, TestInfo } from '@playwright/test'
import type { Workspace } from './fixtures'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { join } from 'node:path'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  headerStatus,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import { startConversation } from './schedule-fixture'

test('fixture reads a run while another process holds the SQLite writer', async ({ workspace }) => {
  const existing = workspace.service.store.runs()[0]
  expect(existing).toBeDefined()
  const script = `import sqlite3, sys
connection = sqlite3.connect(sys.argv[1], timeout=5)
connection.execute('BEGIN IMMEDIATE')
print('ready', flush=True)
sys.stdin.read()
connection.rollback()
`
  const holder = spawn('python3', ['-u', '-c', script, join(workspace.service.config.dataDir, 'manager.db')], { stdio: ['pipe', 'pipe', 'pipe'] })
  const closed = once(holder, 'close')
  try {
    const [ready] = await once(holder.stdout, 'data')
    expect(ready.toString()).toContain('ready')
    expect(workspace.service.store.run(existing.id)).toMatchObject({ id: existing.id })
  }
  finally {
    holder.stdin.end()
    await closed
  }
})

// A failed conversation needs the user while another conversation is working.
async function seed(workspace: Workspace) {
  await workspace.api('/api/chats')
  const agent = workspace.service.agent({ name: 'Ops' })
  const { chat: audit, run } = await startConversation(workspace, 'Nightly security audit', 'Audit dependencies. fixture:fail', agent.id)
  await expect.poll(() => workspace.service.store.run(run.id)?.status, { timeout: 30000 }).toBe('failed')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep watching the build. fixture:chat-hang' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('running')
  return { chat, audit }
}

async function signIn(page: Page) {
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Fil', exact: true }).first()).toBeVisible({ timeout: 30000 })
  await page.getByRole('tab', { name: 'Tout', exact: true }).click()
}

async function capture(page: Page, testInfo: TestInfo, name: string) {
  await page.evaluate(() => document.fonts.ready)
  await page.screenshot({ path: testInfo.outputPath(`${name}.png`) })
}

test('the Fil search toggle closes an empty focused search', async ({ page }) => {
  await signIn(page)
  const toggle = page.getByRole('button', { name: 'Filter the Fil', exact: true })
  const input = page.getByRole('textbox', { name: 'Filter conversations' })
  await toggle.click()
  await expect(input).toBeFocused()
  await expect(toggle).toHaveAttribute('aria-expanded', 'true')
  await toggle.click()
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await expect(input).not.toBeFocused()
})

test.describe('hover previews', () => {
  test.use({ hasTouch: false })

  test('a result preview stays hovered and Escape dismisses visible and pending previews', async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    const chat = await workspace.api('/api/chats', 'POST', {})
    const now = Date.now()
    const chats = [{
      ...chat,
      title: 'Unread result preview',
      status: 'succeeded',
      unread: true,
      finishedAt: now,
      updatedAt: now,
      preview: 'The build passed and the result is ready to read.',
    }, {
      ...chat,
      id: randomUUID(),
      title: 'Read result without a preview',
      status: 'succeeded',
      unread: false,
      finishedAt: now - 1000,
      updatedAt: now - 1000,
      preview: null,
    }]
    await page.route('**/api/chats/stream?**', route => route.fulfill({
      contentType: 'text/event-stream',
      body: `id: 0\nevent: batch\ndata: ${JSON.stringify({
        events: [],
        state: {
          run: null,
          chat: null,
          chats,
          artifacts: [],
        },
        reset: true,
        more: false,
      })}\n\n`,
    }))
    await signIn(page)
    const row = page.locator(`[data-fil-key="chat:${chat.id}"]`)
    const trigger = row.getByRole('button', { name: 'Open “Unread result preview”', exact: true })
    const tooltip = page.getByRole('tooltip')
    await trigger.hover()
    await expect(tooltip).toBeVisible()
    await tooltip.hover()
    await expect(tooltip).toBeVisible()
    await expect(row).toHaveAttribute('data-unread', '')
    await page.keyboard.press('Escape')
    await expect(tooltip).toBeHidden()

    await trigger.focus()
    await expect(tooltip).toBeVisible()
    await page.getByRole('button', { name: 'Open “Read result without a preview”', exact: true }).hover()
    await expect(trigger).toBeFocused()
    await expect(tooltip).toBeVisible()
    await expect(tooltip).toContainText('The build passed and the result is ready to read.')
    await page.keyboard.press('Escape')
    await expect(tooltip).toBeHidden()
    await expect(trigger).toBeFocused()

    await page.getByRole('tab', { name: 'Tout', exact: true }).focus()
    await trigger.focus()
    await page.keyboard.press('Escape')
    // Cover cancellation of the 350 ms opening timer, not only the rendered tooltip.
    await page.waitForTimeout(450)
    await expect(tooltip).toBeHidden()
    await expect(row).toHaveAttribute('data-unread', '')
  })
})

test('delegated conversations keep their requester in attention cards and live rows', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const parent = await workspace.api('/api/chats', 'POST', {})
  const now = Date.now()
  const tasks = [
    {
      id: randomUUID(),
      title: 'Child with a question',
      status: 'succeeded',
      pendingQuestions: 1,
    },
    { id: randomUUID(), title: 'Failed child', status: 'failed' },
    { id: randomUUID(), title: 'Interrupted child', status: 'interrupted' },
    {
      id: randomUUID(),
      title: 'Unread child report',
      status: 'succeeded',
      unread: true,
    },
    { id: randomUUID(), title: 'Working child', status: 'running' },
  ]
  const chats = tasks.map(task => ({
    ...parent,
    agentName: 'Reviewer',
    lifecycle: 'active',
    paused: false,
    pendingQuestions: 0,
    unread: false,
    updatedAt: now,
    finishedAt: task.status === 'running' ? null : now,
    runId: randomUUID(),
    provider: 'claude',
    requester: { chatId: parent.id, title: 'Review the send queue' },
    ...task,
  }))
  await page.route('**/api/chats/stream?**', route => route.fulfill({
    contentType: 'text/event-stream',
    body: `id: 0\nevent: batch\ndata: ${JSON.stringify({
      events: [],
      state: {
        run: null,
        chat: null,
        chats,
        artifacts: [],
      },
      reset: true,
      more: false,
    })}\n\n`,
  }))
  await signIn(page)
  const forYou = page.getByRole('region', { name: 'Tout', exact: true })
  const live = page.getByRole('region', { name: 'Tout', exact: true })
  for (const [index, task] of tasks.entries()) {
    const section = index === tasks.length - 1 ? live : forYou
    const row = section.locator(`[data-fil-key="chat:${task.id}"]`)
    await expect(row.getByTestId('fil-requester')).toHaveText('Delegated by Review the send queue')
    await expect(row.getByRole('img', { name: 'Latest harness: Claude Code', exact: true })).toBeVisible()
    if (task.title === 'Unread child report')
      await expect(row.getByRole('button', { name: `Open “${task.title}”`, exact: true })).toHaveAccessibleDescription('Finished')
  }
})

test('automatic quota waits appear with scheduled resumptions without a deadline', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const quota = {
    ...chat,
    title: 'Waiting on Codex quota',
    status: 'running',
    accountWaitReason: 'Usage exhausted. Waiting for an available Codex account to resume.',
    accountRequired: null,
    agentName: 'Ops',
    pendingQuestions: 0,
  }
  await page.route('**/api/chats/stream?**', route => route.fulfill({
    contentType: 'text/event-stream',
    body: `id: 0\nevent: batch\ndata: ${JSON.stringify({
      events: [],
      state: {
        run: null,
        chat: null,
        chats: [quota],
        artifacts: [],
      },
      reset: true,
      more: false,
    })}\n\n`,
  }))
  await signIn(page)
  const waiting = page.getByRole('region', { name: 'Tout', exact: true })
  const row = waiting.locator(`[data-fil-key="chat:${chat.id}"]`)
  await expect(row.getByText('Waiting for quota', { exact: true })).toBeVisible()
  await expect(row).toHaveAttribute('data-fil-kind', 'waiting')
  await expect(page.locator('[data-fil-kind=running]')).toHaveCount(0)
})

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
    const live = page.getByRole('region', { name: 'Tout', exact: true })
    await expect(live.getByText('Suspended for exclusive work', { exact: true })).toBeVisible()
    await expect(live.getByText('Working', { exact: true })).toHaveCount(0)
    await expect(live.locator('.avatar-orbit')).toHaveCount(0)
    await live.locator(`[data-fil-key="chat:${chat.id}"]`).click()
    await expect(page.getByRole('region', { name: 'Activity output' }).getByText('Suspended for exclusive work.')).toBeVisible()
    await expect(page.locator('.agent-working')).toHaveCount(0)
    // The header names the suspension and stops the avatar's ring.
    await expect(headerStatus(page, 'Suspended for exclusive work')).toBeVisible()
    await expect(page.locator('.chat-header .avatar-orbit').filter({ visible: true })).toHaveCount(0)
    workspace.service.store.updateRun(runId, { suspendedByExclusive: null })
    expect(workspace.service.store.run(runId)?.suspendedByExclusive ?? null).toBeNull()
    // This fixture writes from a separate process. The stream's fallback refresh is 15 seconds.
    await expect(live.getByText('Working', { exact: true })).toBeVisible({ timeout: 20000 })
    await expect(page.locator('.agent-working')).toHaveCount(1)
  }
  finally {
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
    await workspace.api(`/api/chats/${chat.id}`, 'DELETE', { confirm: true })
  }
})

test('the Fil, rail and shortcuts lead through the workspace', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  const { chat, audit } = await seed(workspace)
  await signIn(page)

  const forYou = page.getByRole('region', { name: 'Tout', exact: true })
  await expect(forYou.getByText('Nightly security audit', { exact: true })).toBeVisible()
  await expect(forYou.getByText('Failed', { exact: true })).toBeVisible()
  await forYou.getByRole('button', { name: 'More actions for “Nightly security audit”' }).click()
  await expect(page.getByRole('menuitem', { name: 'Snooze until…', exact: true })).toBeVisible()
  await page.keyboard.press('Escape')
  const live = page.getByRole('region', { name: 'Tout', exact: true })
  await expect(live.getByText('Working', { exact: true })).toBeVisible()
  await expect(live.locator('[data-fil-kind=running]')).toHaveCount(1)
  const rail = page.getByRole('navigation', { name: 'Workspace navigation' })
  const fil = rail.getByRole('link', { name: 'Fil', exact: true })
  await expect(fil).toHaveAttribute('aria-current', 'page')
  await expect(fil).toHaveAttribute('aria-description', '1 to read')
  await expectSingleScroll(page)
  await capture(page, testInfo, 'signal-fil-desktop')

  // J selects the first item and Enter opens it, like a mail client. A mission reads beside the
  // Fil, which stays the current place, and its Mission tab holds the mission itself.
  await page.getByRole('tab', { name: /^À lire/ }).click()
  await page.getByRole('heading', { name: 'Fil', exact: true }).click()
  await page.keyboard.press('j')
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(workspacePath(`/chats/${audit.slug}`))
  await expect(fil).toHaveAttribute('aria-current', 'page')
  await page.getByRole('tab', { name: 'Tout', exact: true }).click()
  await expect(forYou.locator('[aria-current="page"]')).toContainText('Nightly security audit')
  await expect(page.getByRole('heading', { name: 'Nightly security audit' })).toBeVisible()
  await expectSingleScroll(page)

  // G chords move between the three places.
  await page.keyboard.press('g')
  await page.keyboard.press('r')
  await expect(page.getByRole('heading', { name: 'Rules', exact: true, level: 1 })).toBeVisible()
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

  // The palette finds conversations and commands and acts on them directly.
  await page.keyboard.press('Control+k')
  const palette = page.getByRole('dialog', { name: 'Search and commands' })
  await expect(palette).toBeVisible()
  await page.keyboard.type('nightly')
  // What needs the user ranks first; the matching conversation can be opened here.
  await expect(palette.getByRole('option').first()).toContainText('Nightly security audit')
  await capture(page, testInfo, 'signal-palette-desktop')
  await palette.getByRole('option').first().click()
  await expect(palette).toHaveCount(0)
  await expect(page).toHaveURL(workspacePath(`/chats/${audit.slug}`))

  // The conversation keeps the Fil beside it and names the agent's live step.
  await page.keyboard.press('g')
  await page.keyboard.press('f')
  await live.getByRole('button', { name: /^Open “Keep watching the build/ }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${chat.slug}`))
  await expect(page.getByRole('complementary', { name: 'Fil' }).locator('[aria-current="page"]')).toBeVisible()
  const working = page.locator('.agent-working')
  await expect(working).toBeVisible()
  await expect(working).toHaveAttribute('aria-live', 'polite')
  await expect(page.locator('.chat-page .avatar-orbit').filter({ visible: true })).toHaveCount(1)
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
  const { chat, audit } = await seed(workspace)
  await page.setViewportSize({ width: 390, height: 844 })
  await signIn(page)
  const dock = page.getByRole('navigation', { name: 'Quick navigation' })
  await expect(dock).toBeVisible()
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expect(page.getByRole('region', { name: 'Tout', exact: true }).getByText('Nightly security audit', { exact: true }).first()).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
  await expectSingleScroll(page)
  await capture(page, testInfo, 'signal-fil-mobile')

  await dock.getByRole('link', { name: 'Rules' }).click()
  await expect(page.getByRole('heading', { name: 'Rules', exact: true, level: 1 })).toBeVisible()
  await dock.getByRole('link', { name: 'Atelier' }).click()
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Sign out' })).toBeVisible()
  await capture(page, testInfo, 'signal-atelier-mobile')
  await dock.getByRole('link', { name: 'Fil' }).click()

  await page.locator(`[data-fil-key="chat:${chat.id}"]`).getByRole('button', { name: /^Open “/ }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${chat.slug}`))
  await expect(dock).toHaveCount(0)
  await expect(page.locator('.agent-working')).toBeVisible()
  await capture(page, testInfo, 'signal-conversation-mobile')
  await page.getByRole('link', { name: 'Back to the Fil' }).click()
  await expect(page).toHaveURL(workspacePath('/'))
  await expect(dock).toBeVisible()

  // A failed conversation opens full screen from the Fil and returns to it.
  // Earlier journeys in this worker seeded the same mission name; open this journey's own card.
  await page.locator(`[data-fil-key="chat:${audit.id}"]`).getByRole('button', { name: 'Open “Nightly security audit”' }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${audit.slug}`))
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

test('snoozing a blocked conversation clears attention until it is restored', async ({ page, workspace }) => {
  await workspace.api('/api/chats')
  const chat = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { id: chat.id, title: 'Review is blocked' })
  const run = workspace.service.chatRun(chat.id, 'Review is blocked')
  workspace.service.store.updateRun(run.id, {
    outcome: {
      status: 'blocked',
      waitingOn: 'external',
      reason: 'Waiting for CI',
      evidence: [],
      reportedAt: Date.now(),
    },
  })
  await signIn(page)
  const row = page.locator(`[data-fil-key="chat:${chat.id}"]`)
  await expect(row).toContainText('Blocked')
  await row.getByRole('button', { name: /^More actions/ }).click()
  await page.getByRole('menuitem', { name: 'Snooze until…' }).click()
  await page.getByRole('button', { name: 'Snooze', exact: true }).click()
  await expect(row).toHaveAttribute('data-fil-kind', 'snoozed')
  await expect.poll(async () => (await workspace.api('/api/chats')).find((entry: any) => entry.id === chat.id).snoozedUntil).toBeGreaterThan(Date.now())
  await row.getByRole('button', { name: /^More actions/ }).click()
  await page.getByRole('menuitem', { name: 'Restore' }).click()
  await expect(row).toHaveAttribute('data-fil-kind', 'review')

  await row.getByRole('button', { name: /^Mark .* as read$/ }).click()
  await expect.poll(async () => (await workspace.api('/api/chats')).find((entry: any) => entry.id === chat.id).unread).toBe(false)
  await page.getByRole('tab', { name: 'Tout', exact: true }).click()
  await row.getByRole('button', { name: /^More actions/ }).click()
  await page.getByRole('menuitem', { name: 'Snooze until…' }).click()
  await page.getByRole('button', { name: 'Snooze', exact: true }).click()
  await expect(row).toHaveAttribute('data-fil-kind', 'snoozed')
  await row.getByRole('button', { name: /^More actions/ }).click()
  await page.getByRole('menuitem', { name: 'Mark as read' }).click()
  await expect(row).not.toHaveAttribute('data-fil-kind', 'snoozed')
})
