import type { Page, TestInfo } from '@playwright/test'
import type { Workspace } from './fixtures'
import { randomUUID } from 'node:crypto'
import { expect, expectSingleScroll, test } from './fixtures'

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
  await page.goto('/')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  // Password verification in the native debug backend can take several seconds.
  await expect(page.getByRole('heading', { name: 'Fil', exact: true }).first()).toBeVisible({ timeout: 30000 })
}

async function capture(page: Page, testInfo: TestInfo, name: string) {
  await page.evaluate(() => document.fonts.ready)
  await page.screenshot({ path: testInfo.outputPath(`${name}.png`) })
}

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

  // J selects the first item and Enter opens it, like a mail client.
  await page.keyboard.press('j')
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(`/runs/${run.id}`)
  await expect(rail.getByRole('link', { name: 'Atelier', exact: true })).toHaveAttribute('aria-current', 'page')

  // G chords move between the three places.
  await page.keyboard.press('g')
  await page.keyboard.press('m')
  await expect(page.getByRole('heading', { name: 'Missions' })).toBeVisible()
  await page.keyboard.press('g')
  await page.keyboard.press('a')
  await expect(page.getByRole('heading', { name: 'Atelier', exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: /Agents/ })).toBeVisible()
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
  await expect(page).toHaveURL(`/chats/${chat.id}`)
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
  await expect(page).toHaveURL('/chats')
  await expect(page.getByRole('heading', { name: 'What are we building?' })).toBeVisible()
  // Free the runner for the next journey in this worker.
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})

test('phones use the floating dock and read conversations full screen', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  const { chat } = await seed(workspace)
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
  await expect(page).toHaveURL(`/chats/${chat.id}`)
  await expect(dock).toHaveCount(0)
  await expect(page.locator('.agent-working')).toBeVisible()
  await capture(page, testInfo, 'signal-conversation-mobile')
  await page.getByRole('link', { name: 'Back to the Fil' }).click()
  await expect(page).toHaveURL('/')
  await expect(dock).toBeVisible()
})
