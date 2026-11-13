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

for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
  test(`conversation read checks act directly at ${viewport.width}px`, async ({ page, workspace }) => {
    test.setTimeout(150000)
    const { chat, notes } = await seed(workspace)
    await page.setViewportSize(viewport)
    await page.goto(workspacePath('/'))
    await authenticateWorkspace(page)
    const row = page.locator(`[data-fil-key="chat:${chat.id}"]`)
    await expect(row).toHaveAttribute('data-unread', '')
    await expect(row.getByRole('button', { name: /^More actions/ })).toHaveCount(0)
    const check = row.getByRole('button', { name: /^Mark “.*” as read$/ })
    await check.focus()
    await expect(check).toBeVisible()
    const location = page.url()
    await check.click()
    await expect.poll(() => unreadOf(workspace, chat.id)).toBe(false)
    await expect(row).toHaveCount(0)
    expect(page.url()).toBe(location)
    await expect(page.getByRole('menu')).toHaveCount(0)

    await page.getByRole('tab', { name: 'Tout', exact: true }).click()
    await expect(row).toBeVisible()
    await expect(row).not.toHaveAttribute('data-unread')
    await expect(row.getByRole('button')).toHaveCount(1)
    await expect(row.getByRole('button', { name: /^Open “/ })).toBeVisible()

    const mission = page.locator(`[data-fil-key="task:${notes.id}"]`)
    await mission.getByRole('button', { name: /^More actions/ }).click()
    await expect(page.getByRole('menuitem', { name: 'Mark as read', exact: true })).toBeVisible()
    await expect(page.getByRole('menuitem', { name: 'Run again', exact: true })).toBeVisible()
    await expect(page.getByRole('menuitem', { name: 'Archive mission', exact: true })).toBeVisible()
  })
}

test.describe('native touch search selection', () => {
  test.use({ hasTouch: true, isMobile: true })

  test('a touched search action survives live regrouping and touch cancellation', async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    await workspace.api('/api/chats')
    const finished = await workspace.api('/api/chats', 'POST', {})
    await workspace.api(`/api/chats/${finished.id}/messages`, 'POST', { id: randomUUID(), text: 'Native touch finished result' })
    await expect.poll(() => workspace.service.chats.detail(finished.id).run?.status).toBe('succeeded')
    await page.goto(workspacePath('/chats'))
    await authenticateWorkspace(page)
    const composer = page.getByRole('textbox', { name: 'Message', exact: true })
    await composer.fill('Keep the touch draft until selection')
    await page.getByRole('tab', { name: 'Tout', exact: true }).click()
    await page.getByRole('button', { name: 'Search workspace', exact: true }).click()
    const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
    await palette.getByRole('combobox', { name: 'Search', exact: true }).fill('Native touch')
    await page.setViewportSize({ width: 390, height: 844 })
    // This Chromium project can hold a real touch across the live update. A tap
    // also delivers compatibility mouse/click events after its pointerup.
    const touch = await page.context().newCDPSession(page)
    for (const cancel of [true, false]) {
      const title = `Native touch decoy ${cancel ? 'cancel' : 'choose'}`
      const decoy = await workspace.api('/api/chats', 'POST', {})
      await workspace.api(`/api/chats/${decoy.id}`, 'PUT', { title, revision: decoy.revision })
      const decoyOption = palette.getByRole('option', { name: new RegExp(`^${title}`) })
      await expect(decoyOption).toBeVisible()
      const filDecoy = page.getByRole('button', { name: `Open “${title}”`, exact: true, includeHidden: true })
      await expect(filDecoy).toHaveCount(1)
      const start = palette.getByRole('option', { name: /^Start a conversation:/ })
      const bounds = await start.boundingBox()
      expect(bounds).not.toBeNull()
      const point = { x: bounds!.x + bounds!.width / 2, y: bounds!.y + bounds!.height / 2 }
      await touch.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [point] })
      await workspace.api(`/api/chats/${decoy.id}`, 'DELETE', {})
      await expect(filDecoy).toHaveCount(0)
      await touch.send('Input.dispatchTouchEvent', { type: cancel ? 'touchCancel' : 'touchEnd', touchPoints: [] })
      if (cancel) {
        await expect(palette).toBeVisible()
        await expect(composer).toHaveValue('Keep the touch draft until selection')
        await expect(decoyOption).toHaveCount(0)
      }
    }

    await expect(palette).toBeHidden()
    await expect(composer).toHaveValue('Native touch')
    await touch.detach()
  })
})
