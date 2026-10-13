import type { Page, TestInfo } from '@playwright/test'
import type { Workspace } from './fixtures'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
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

test('finished work waits in For you until it is read, on the desktop and the phone', async ({ page, workspace }, testInfo) => {
  test.setTimeout(150000)
  const { chat, done, notes } = await seed(workspace)
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Fil', exact: true }).first()).toBeVisible({ timeout: 30000 })

  const forYou = page.getByRole('region', { name: 'For you', exact: true })
  const conversation = forYou.locator(`[data-fil-key="chat:${chat.id}"]`)
  const mission = forYou.locator(`[data-fil-key="task:${notes.id}"]`)
  await expect(conversation).toHaveAttribute('data-unread', '')
  await expect(conversation.getByText('Finished', { exact: true })).toBeVisible()
  await expect(conversation.getByText(/The approach looks good\. Draft the changelog for 0\.53\./)).toBeVisible()
  await expect(conversation.getByRole('img', { name: 'Unread' })).toBeVisible()
  await expect(mission.getByText('Mission finished', { exact: true })).toBeVisible()
  // What needs an answer stays above what only waits to be read.
  await expect(forYou.locator('article').first()).toContainText('Nightly security audit')
  await expect(page.getByText('1 needs you · 2 to read')).toBeVisible()
  const rail = page.getByRole('navigation', { name: 'Workspace navigation' })
  await expect(rail.getByRole('link', { name: 'Fil', exact: true })).toHaveAttribute('aria-description', '1 need you · 2 to read')
  await expectSingleScroll(page)
  await capture(page, testInfo, 'unread-fil-desktop-dark')
  await page.emulateMedia({ colorScheme: 'light' })
  await capture(page, testInfo, 'unread-fil-desktop-light')

  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.getByRole('navigation', { name: 'Quick navigation' })).toBeVisible()
  await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
  await capture(page, testInfo, 'unread-fil-mobile-dark')
  await page.setViewportSize({ width: 1440, height: 900 })

  // Reading the conversation marks it read for the account; it stays in place, read, while open.
  await conversation.getByRole('button', { name: /^Read “/ }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${chat.slug}`))
  await expect.poll(() => unreadOf(workspace, chat.id)).toBe(false)
  const beside = page.getByRole('complementary', { name: 'Fil' }).getByRole('region', { name: 'For you', exact: true })
  const opened = beside.locator(`[data-fil-key="chat:${chat.id}"]`)
  await expect(opened.getByText('Read', { exact: true })).toBeVisible()
  await expect(opened).not.toHaveAttribute('data-unread')
  await expect(opened.getByRole('button', { name: /^Open “/ })).toBeVisible()
  await expect(opened.getByRole('button', { name: /^Mark “.*” as read$/ })).toHaveCount(0)
  await capture(page, testInfo, 'unread-reading-desktop-dark')

  // Back in the Fil, it moved to Recent; Mark all read clears the rest.
  await page.getByRole('link', { name: 'Fil', exact: true }).first().click()
  await expect(page).toHaveURL(workspacePath('/'))
  await expect(page.getByRole('region', { name: 'Recent', exact: true }).locator(`[data-fil-key="chat:${chat.id}"]`)).toBeVisible()
  await expect(conversation).toHaveCount(0)
  await forYou.getByRole('button', { name: 'Mark 1 finished item as read' }).click()
  await expect(mission).toHaveCount(0)
  await expect.poll(async () => (await workspace.api('/api/tasks/activity')).find((run: { id: string }) => run.id === done.id)?.unread).toBe(false)
  await expect(page.getByText('1 needs you', { exact: true })).toBeVisible()
  await expect(rail.getByRole('link', { name: 'Fil', exact: true })).toHaveAttribute('aria-description', '1 need you')
  await capture(page, testInfo, 'unread-cleared-desktop-dark')
})
