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

// Finished work the user has not opened: a conversation turn and a mission run that succeeded,
// beside a failed mission that still needs an answer.
async function seed(workspace: Workspace) {
  // The account starts reading before the agents finish; earlier history counts as read.
  await workspace.api('/api/chats')
  await workspace.api('/api/tasks/activity')
  const agent = workspace.service.agent({ name: 'Writer' })
  const mission = (name: string, prompt: string) => workspace.service.task({
    name,
    prompt,
    agentId: agent.id,
    projectId: null,
    skills: null,
    worktree: false,
    enabled: false,
    cron: null,
  })
  const notes = mission('Weekly release notes', 'Summarize what shipped this week.')
  const audit = mission('Nightly security audit', 'Audit dependencies. fixture:fail')
  const done = await workspace.service.enqueue(notes.id)
  const failed = await workspace.service.enqueue(audit.id)
  await expect.poll(() => workspace.service.store.run(done.id)?.status, { timeout: 30000 }).toBe('succeeded')
  await expect.poll(() => workspace.service.store.run(failed.id)?.status, { timeout: 30000 }).toBe('failed')

  const chat = await workspace.api('/api/chats', 'POST', { agentId: agent.id })
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Draft the changelog for 0.53.' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).status, { timeout: 30000 }).toBe('succeeded')
  return { chat, done, notes }
}

async function capture(page: Page, testInfo: TestInfo, name: string) {
  await page.evaluate(() => document.fonts.ready)
  // Let cards finish entering before the capture.
  await page.waitForTimeout(600)
  await page.screenshot({ path: testInfo.outputPath(`${name}.png`) })
}

const unreadOf = async (workspace: Workspace, id: string) => (await workspace.api('/api/chats')).find((chat: { id: string }) => chat.id === id)?.unread

test('compact tabs acknowledge results and errors on desktop and phone', async ({ page, workspace }, testInfo) => {
  test.setTimeout(150000)
  const { chat, done, notes } = await seed(workspace)
  await useRelayForHttpMocks(page)
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  const inbox = page.getByRole('region', { name: 'À lire', exact: true })
  const conversation = inbox.locator(`[data-fil-key="chat:${chat.id}"]`)
  await expect(conversation).toHaveAttribute('data-unread', '')
  await expect(conversation.getByRole('img', { name: 'Unread' })).toBeVisible()
  await expect(page.locator('.fil-preview')).toHaveCount(0)
  await expect(inbox.getByText('Nightly security audit', { exact: true })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'À lire 3', exact: true })).toBeVisible()
  const rail = page.getByRole('navigation', { name: 'Workspace navigation' })
  await expect(rail.getByRole('link', { name: 'Fil', exact: true })).toHaveAttribute('aria-description', '3 to read')
  await expectSingleScroll(page)
  await capture(page, testInfo, 'compact-fil-desktop-dark')
  await page.emulateMedia({ colorScheme: 'light' })
  await capture(page, testInfo, 'compact-fil-desktop-light')
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.getByRole('navigation', { name: 'Quick navigation' })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
  await capture(page, testInfo, 'compact-fil-mobile')
  await page.setViewportSize({ width: 1440, height: 900 })

  let release!: () => void
  let requested!: () => void
  const held = new Promise<void>(resolve => release = resolve)
  const started = new Promise<void>(resolve => requested = resolve)
  const streamRoute = `**/api/chats/${chat.slug}/stream*`
  await page.route(streamRoute, async (route) => {
    requested()
    await held
    await route.continue()
  })
  try {
    await conversation.getByRole('button', { name: /^Open “/ }).click()
    await started
    expect(await unreadOf(workspace, chat.id)).toBe(true)
    release()
    await expect.poll(() => unreadOf(workspace, chat.id)).toBe(false)
  }
  finally {
    release()
    await page.unroute(streamRoute)
  }

  await expect(conversation).toHaveCount(0)
  await page.getByRole('tab', { name: 'Tout', exact: true }).click()
  const all = page.getByRole('region', { name: 'Tout', exact: true })
  await expect(all.locator(`[data-fil-key="chat:${chat.id}"]`)).not.toHaveAttribute('data-unread')
  await expect(all.locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText('Finished')

  // A failed mission is acknowledged on opening, without retrying or changing its status.
  const failed = all.locator('[data-fil-kind="failed-mission"]').filter({ hasText: 'Nightly security audit' })
  await failed.getByRole('button', { name: /^Open “/ }).click()
  await expect(failed).not.toHaveAttribute('data-unread')
  await expect(failed).toHaveAttribute('data-fil-kind', 'failed-mission')
  await page.getByRole('button', { name: 'Mark all as read', exact: true }).click()
  await expect.poll(async () => (await workspace.api('/api/tasks/activity')).find((run: { id: string }) => run.id === done.id)?.unread).toBe(false)
  await expect(all.locator(`[data-fil-key="task:${notes.id}"]`)).toBeVisible()
  await page.getByRole('tab', { name: 'À lire', exact: true }).click()
  await expect(inbox.locator('[data-fil-key]')).toHaveCount(0)
  await capture(page, testInfo, 'compact-fil-all-read')
})

test('an open mission acknowledges completion from any tab after syncing', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const { done } = await seed(workspace)
  workspace.service.store.updateRun(done.id, { status: 'running', finishedAt: null })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/runs/${done.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: /^Files/ }).click()
  workspace.service.store.updateRun(done.id, { status: 'succeeded', finishedAt: Date.now(), summary: 'Output waiting to be read' })
  workspace.service.store.event(done.id, 'run.completed', 'Mission completed')
  const unread = async () => (await workspace.api('/api/tasks/activity')).find((run: { id: string }) => run.id === done.id)?.unread
  await expect(page.getByText('Execution finished', { exact: true }).first()).toBeVisible({ timeout: 20000 })
  await expect.poll(unread).toBe(false)
})
