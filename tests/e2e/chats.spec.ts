import { randomUUID } from 'node:crypto'
import {
  expect,
  expectChatReady,
  expectSingleScroll,
  initializeRepository,
  test,
} from './fixtures'

test('a transient agent error shows a retry countdown and resumes without user input', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:agent-failures:[{"message":"workspace routing discovery timed out"}]',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.retry?.attempt).toBe(1)
  const waiting = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(waiting.status).toBe('queued')
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  const notice = page.getByRole('status').filter({ hasText: /Temporary agent error\. Retrying in \d+ seconds \(attempt 1\/3\)\./ })
  await expect(notice).toBeVisible()
  const before = await notice.textContent()
  await expect.poll(() => notice.textContent()).not.toBe(before)
  await page.screenshot({ path: test.info().outputPath('agent-retry-countdown.png') })
  expect((await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(false)
  workspace.service.store.updateRun(waiting.id, { retry: { ...waiting.retry, nextAttemptAt: Date.now() } })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  await expect(page.getByText('Ready for the next step.', { exact: false }).last()).toBeVisible()
  await expect(notice).toHaveCount(0)
})

test('queued Claude conversation explains the missing account and continues after sign-in', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep this request while connecting', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountRequired).toBe('claude')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.accountWaitReason).toBe('Connect a Claude Code account in Connections before running this agent.')
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.setViewportSize({ width: 390, height: 844 })
  // Password verification in the native debug backend can take several seconds.
  await expect(page.getByText('Claude Code account needed', { exact: true })).toBeVisible({ timeout: 30000 })
  await expect(page.getByText('Waiting for a Claude Code account', { exact: true })).toBeVisible()
  await expect(page.getByText('Your agent is working…', { exact: true })).toHaveCount(0)
  await expect(page.getByText('Starting agent…', { exact: true })).toHaveCount(0)
  await page.getByRole('link', { name: 'Open Connections', exact: true }).click()
  await expect(page).toHaveURL('/connections')
  await page.getByRole('button', { name: 'Add Claude Code account', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Continue to sign-in' }).click()
  const dialog = page.getByRole('dialog', { name: 'Connect your Claude account' })
  await dialog.getByLabel('Claude authorization code').fill('fixture-code')
  await dialog.getByRole('button', { name: 'Finish sign-in' }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.accountRequired).toBeNull()
  await page.goto(`/chats/${chat.id}`)
  await expect(page.getByText('Claude fixture completed', { exact: true })).toBeVisible()
  await expect(page.getByText('Claude Code account needed', { exact: true })).toHaveCount(0)
  await expect(page.locator('.activity-message').getByText('Keep this request while connecting', { exact: true })).toHaveCount(1)
})

test('conversation switcher preserves drafts and supports search, keyboard and mobile sheets', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await workspace.restart()
  const first = await workspace.api('/api/chats', 'POST', {})
  const second = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...first, title: 'Simplify the chat navigation', updatedAt: Date.now() })
  workspace.service.store.put('chats', { ...second, title: 'Review the release notes', updatedAt: Date.now() - 86400000 })
  await page.goto(`/chats/${first.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Simplify the chat navigation', exact: true })).toBeVisible()
  const message = page.getByRole('textbox', { name: 'Message', exact: true })
  await message.fill('Keep this unsent draft')
  const trigger = page.getByRole('button', { name: 'Conversations', exact: true })
  const dialog = page.getByRole('dialog', { name: /Conversations/ })
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1440, 390, 320]) {
      await page.setViewportSize({ width, height: width === 320 ? 600 : 900 })
      await trigger.click()
      await expect(dialog).toBeVisible()
      await expect(page.getByRole('searchbox', { name: 'Search conversations' })).toBeFocused()
      const box = await dialog.boundingBox()
      expect(box!.x).toBeGreaterThanOrEqual(0)
      expect(box!.x + box!.width).toBeLessThanOrEqual(width)
      if (width < 641)
        expect(Math.round(box!.y + box!.height)).toBe(width === 320 ? 600 : 900)
      await expect(dialog.getByRole('link', { name: /Simplify the chat navigation/ })).toHaveAttribute('aria-current', 'page')
      await page.screenshot({ path: test.info().outputPath(`switcher-${colorScheme}-${width}.png`), animations: 'disabled' })
      await page.keyboard.press('Shift+Tab')
      expect(await page.evaluate(() => !!document.activeElement?.closest('dialog'))).toBe(true)
      await page.keyboard.press('Escape')
      await expect(dialog).toHaveCount(0)
      await expect(trigger).toBeFocused()
      await expect(message).toHaveValue('Keep this unsent draft')
      await expectSingleScroll(page)
      await expect(message).toBeInViewport()
    }
  }

  await trigger.click()
  const search = page.getByRole('searchbox', { name: 'Search conversations' })
  await search.fill('nothing-matches-this')
  await expect(dialog.getByRole('status')).toHaveText('No conversations match your search.')
  await search.fill('release notes')
  await expect(dialog.getByRole('link')).toHaveCount(1)
  await dialog.getByRole('link', { name: /Review the release notes/ }).click()
  await expect(page).toHaveURL(`/chats/${second.id}`)
  await expect(dialog).toHaveCount(0)
  await expect(message).toHaveValue('')
  await trigger.click()
  await dialog.getByRole('link', { name: /Simplify the chat navigation/ }).click()
  await expect(message).toHaveValue('Keep this unsent draft')
  await trigger.click()
  await dialog.getByRole('button', { name: 'New conversation', exact: true }).click()
  await expect(page).toHaveURL('/chats')
  await expect(dialog).toHaveCount(0)
  await expect(message).toHaveValue('')
  await workspace.restart()
})

test('compact mobile toolbar keeps details and workspace controls accessible', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the mobile navigation and keep the conversation easy to find.' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  const details = page.getByRole('button', { name: 'Chat details', exact: true })
  const panel = page.getByRole('dialog', { name: 'Chat details', exact: true })
  const message = page.getByRole('textbox', { name: 'Message', exact: true })
  await message.fill('Keep my draft while browsing details')
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 600 }, { width: 600, height: 390 }]) {
      await page.setViewportSize(viewport)
      await expect(page.locator('.topbar')).not.toBeVisible()
      await expect(page.locator('.activity-toolbar')).not.toBeVisible()
      await expect(page.getByRole('button', { name: 'Conversations', exact: true })).toBeInViewport()
      await expect(message).toBeInViewport()
      const header = await page.locator('.chat-header').boundingBox()
      // Layout can report sub-pixel heights such as 60.0000007.
      expect(Math.round(header!.height)).toBeLessThanOrEqual(60)
      expect(header!.y).toBeLessThanOrEqual(1)
      await expectSingleScroll(page)
      await page.screenshot({ path: test.info().outputPath(`compact-chat-${colorScheme}-${viewport.width}.png`), animations: 'disabled' })
      await details.click()
      await expect(panel).toBeVisible()
      await expect(panel.getByText('Ready', { exact: true })).toBeVisible()
      expect(Math.round((await panel.boundingBox())!.y + (await panel.boundingBox())!.height)).toBe(viewport.height)
      await page.screenshot({ path: test.info().outputPath(`compact-details-${colorScheme}-${viewport.width}.png`), animations: 'disabled' })
      await page.keyboard.press('Escape')
      await expect(panel).toHaveCount(0)
      await expect(details).toBeFocused()
      await expect(message).toHaveValue('Keep my draft while browsing details')
    }
  }

  await page.setViewportSize({ width: 390, height: 844 })
  await details.click()
  await panel.getByRole('button', { name: 'Search conversations', exact: true }).click()
  await expect(page.getByRole('searchbox', { name: 'Search conversations' })).toBeFocused()
  await page.keyboard.press('Escape')
  await details.click()
  await panel.getByRole('button', { name: 'Open activity fullscreen', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Fullscreen activity' })).toBeVisible()
  await page.getByRole('button', { name: 'Exit fullscreen', exact: true }).click()
  await expect(details).toBeFocused()
  await details.click()
  await panel.getByRole('button', { name: 'Follow latest output', exact: true }).click()
  await expect(panel).toHaveCount(0)
  await details.click()
  await panel.getByRole('button', { name: 'Search workspace', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Search and commands' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(details).toBeFocused()
  await details.click()
  await panel.getByRole('button', { name: /Appearance:/ }).click()
  await panel.locator('.theme-popover').getByText('Dark', { exact: true }).click()
  await expect(panel.getByRole('button', { name: 'Appearance: dark' })).toBeVisible()
  await page.keyboard.press('Escape')
  await details.click()
  await panel.getByRole('button', { name: 'Workspace navigation', exact: true }).click()
  await expect(page).toHaveURL('/')
  await expect(page.getByRole('navigation', { name: 'Quick navigation' })).toBeVisible()
  await page.goBack()
  await expect(message).toHaveValue('Keep my draft while browsing details')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeVisible()
  await expect(page.locator('.activity-toolbar')).toBeVisible()
  await expect(details).not.toBeVisible()
  await expect(message).toHaveValue('Keep my draft while browsing details')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('link', { name: 'New chat', exact: true }).click()
  await expect(page).toHaveURL('/chats')
  await details.click()
  await expect(panel.getByText('New conversation', { exact: true })).toBeVisible()
  await expect(panel.getByRole('button', { name: 'Open activity fullscreen' })).toHaveCount(0)
  await page.keyboard.press('Escape')
  await expectSingleScroll(page)
  // This standalone journey must not consume later chats' request budget.
  await workspace.restart()
})

test('ordinary sends and steering stay in the transcript while only follow-ups queue', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  const composer = page.getByRole('textbox', { name: 'Message', exact: true })
  const userMessages = page.locator('.activity-message').filter({ has: page.getByText('You', { exact: true }) })
  const first = 'Review the navigation. fixture:slow-delivery fixture:chat-hang'
  let release!: () => void
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(`**/api/chats/${chat.id}/messages`, async (route) => {
    await gate
    await route.continue()
  })
  await composer.fill(first)
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  try {
    await expect(userMessages.filter({ hasText: first }).getByRole('status')).toHaveText('Sending…')
    await expect(page.getByTestId('conversation-queue')).toHaveCount(0)
    await expectSingleScroll(page)
    await page.screenshot({ path: test.info().outputPath('normal-send-mobile.png'), animations: 'disabled' })
  }
  finally { release() }

  await expect(userMessages.filter({ hasText: first }).getByRole('status')).toHaveText('Starting agent…')
  await expect(composer).toHaveValue('')
  await page.reload()
  await expect(userMessages.filter({ hasText: first }).getByRole('status')).toHaveText('Starting agent…')
  await expect(page.getByTestId('conversation-queue')).toHaveCount(0)
  await page.screenshot({ path: test.info().outputPath('agent-starting-mobile.png'), animations: 'disabled' })
  await expect(userMessages.filter({ hasText: first }).getByRole('status')).toHaveCount(0, { timeout: 15000 })
  await expect(userMessages.filter({ hasText: first })).toHaveCount(1)
  await composer.fill('A follow-up for the next reply')
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await expect(userMessages.filter({ hasText: 'A follow-up for the next reply' })).toHaveCount(0)
  const steer = 'Focus on accessibility. fixture:slow-delivery'
  await composer.fill(steer)
  await page.getByRole('button', { name: 'Steer now', exact: true }).click()
  await expect(userMessages.filter({ hasText: steer }).getByRole('status')).toHaveText('Sending to agent…')
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.screenshot({ path: test.info().outputPath('steer-and-real-queue-mobile.png'), animations: 'disabled' })
  await expect(userMessages.filter({ hasText: steer }).getByRole('status')).toHaveCount(0, { timeout: 15000 })
  await expect(userMessages.filter({ hasText: steer })).toHaveCount(1)
  await page.getByRole('button', { name: 'Stop response', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()
  await composer.fill('Wait until I resume')
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('conversation-queue').getByRole('status')).toHaveText('Queue paused · nothing is sent · 2')
  await expect(userMessages.filter({ hasText: 'Wait until I resume' })).toHaveCount(0)
  await page.screenshot({ path: test.info().outputPath('paused-queue-mobile.png'), animations: 'disabled' })
  await workspace.restart()
})

test('a rejected ordinary send preserves its draft and removes the optimistic message', async ({ page, workspace }) => {
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.route(`**/api/chats/${chat.id}/messages`, route => route.fulfill({ status: 503, contentType: 'application/json', body: JSON.stringify({ error: 'Temporarily unavailable' }) }))
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Keep this draft on failure')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.getByRole('alert')).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Keep this draft on failure')
  await expect(page.locator('.activity-message')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeEnabled()
  await workspace.restart()
})

test('task outcomes stay with the reply and expose evidence without crowding the composer', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const messageId = randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: messageId, text: 'Review the Android fix and report the checks.' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  const run = workspace.service.chats.detail(chat.id).run!
  const artifactPath = `/api/runs/${run.id}/artifacts/${randomUUID()}`
  const outcome = {
    status: 'completed' as const,
    reason: 'Android fix pushed to main. Signed APK published and checks passed.',
    evidence: [
      'Changes pushed to **main** · https://github.com/leo91000/leo-agent-manager/commit/2b62b45',
      '**Local validation passed**\n\n73 tests · lint · debug & release builds',
      '**Android CI passed** · https://github.com/leo91000/leo-agent-manager/actions/runs/35315999841',
      `Published APK: ${artifactPath}`,
      '[Review notes](https://example.test/review) · [Unsafe link](javascript:alert(1))',
    ],
    reportedAt: Date.now(),
    messageId,
  }
  workspace.service.store.updateRun(run.id, { outcome })
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  const panel = page.getByRole('region', { name: 'Task outcome', exact: true })
  await expect(panel.getByText('Task completed', { exact: true })).toBeVisible()
  expect(await panel.evaluate(element => !!element.closest('.activity-scroll'))).toBe(true)
  expect(await panel.evaluate(element => element.previousElementSibling?.classList.contains('activity-message'))).toBe(true)
  await expect(page.getByTestId('agent-actions').filter({ hasText: 'Followed the run' })).toHaveCount(0)
  expect(await panel.evaluate(element => element.parentElement?.lastElementChild === element)).toBe(true)
  const toggle = panel.getByRole('button', { name: 'View evidence', exact: true })
  await expect(toggle).toHaveAttribute('aria-expanded', 'false')
  await expect(panel.getByText(outcome.reason, { exact: true })).toHaveCount(0)
  await toggle.focus()
  await page.keyboard.press('Enter')
  await expect(panel.getByRole('button', { name: 'Hide evidence' })).toHaveAttribute('aria-expanded', 'true')
  await expect(panel.getByText('73 tests · lint · debug & release builds')).toBeVisible()
  await expect(panel.getByRole('link', { name: 'View commit' })).toHaveAttribute('href', 'https://github.com/leo91000/leo-agent-manager/commit/2b62b45')
  await expect(panel.getByRole('link', { name: 'View workflow' })).toHaveAttribute('target', '_blank')
  await expect(panel.getByRole('link', { name: 'View workflow' })).toHaveAttribute('rel', 'noopener noreferrer')
  await expect(panel.getByRole('link', { name: 'Review notes' })).toHaveAttribute('href', 'https://example.test/review')
  await expect(panel.getByRole('link', { name: 'View file' })).toHaveAttribute('href', artifactPath)
  await expect(panel.locator('a[href^="javascript:"]')).toHaveCount(0)
  await panel.getByRole('button', { name: 'Hide evidence' }).blur()
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 1000 })
      await panel.scrollIntoViewIfNeeded()
      await expectSingleScroll(page)
      await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeInViewport()
      await page.screenshot({ path: test.info().outputPath(`outcome-expanded-${width}-${colorScheme}.png`), animations: 'disabled' })
    }
  }

  await panel.getByRole('button', { name: 'Hide evidence' }).click()
  await expect(panel.getByRole('link', { name: 'View commit' })).toHaveCount(0)
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: test.info().outputPath('outcome-collapsed-desktop-dark.png'), animations: 'disabled' })
  // The next turn must never inherit the previous reply's completion report.
  // The run is reused and already succeeded, so wait for this turn to be delivered and finish;
  // otherwise its completion can overwrite the outcomes written below.
  const previousFinish = workspace.service.chats.detail(chat.id).run?.finishedAt ?? 0
  const nextId = randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: nextId, text: 'Another reply without a completion report' })
  await expect.poll(() => {
    const detail = workspace.service.chats.detail(chat.id)
    return !detail.messages.some(message => message.id === nextId && message.status !== 'delivered')
      && detail.run?.status === 'succeeded'
      && (detail.run.finishedAt ?? 0) > previousFinish
  }).toBe(true)
  await page.reload()
  await expect(panel).toHaveCount(0)
  // Outcomes without evidence still expose their reason; attention stays visible.
  for (const status of ['completed', 'blocked', 'needs_input'] as const) {
    workspace.service.store.updateRun(run.id, {
      outcome: {
        ...outcome,
        status,
        reason: 'A concrete explanation of this result.',
        evidence: [],
        reportedAt: Date.now(),
      },
    })
    await page.reload()
    await expect(panel.getByRole('button', { name: 'View details' })).toBeVisible()
    if (status === 'completed') {
      await expect(panel.getByText('A concrete explanation of this result.')).toHaveCount(0)
      await panel.getByRole('button', { name: 'View details' }).click()
    }

    await expect(panel.getByText('A concrete explanation of this result.')).toBeVisible()
  }

  workspace.service.store.updateRun(run.id, { status: 'failed' })
  await page.reload()
  await expect(panel).toHaveCount(0)
})

test('failed replies show the current error instead of an old answer and can be dismissed', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'A successful first reply' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  const firstRun = workspace.service.chats.detail(chat.id).run!
  workspace.service.store.event(firstRun.id, 'error', 'Validation needs attention.', { message: 'Validation needs attention.' })
  workspace.service.store.event(firstRun.id, 'status', 'interrupted')
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.getByRole('note').filter({ hasText: 'Validation needs attention.' })).toBeVisible()
  await expect(page.getByRole('note').filter({ hasText: 'Interrupted' })).toBeVisible()
  await expect(page.getByTestId('agent-actions').filter({ hasText: 'Followed the run' })).toHaveCount(0)
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await expectSingleScroll(page)
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeInViewport()
  }

  // Diagnostic run activity still has the complete lifecycle records.
  await page.goto(`/runs/${firstRun.id}`)
  await page.getByTestId('agent-actions').filter({ hasText: '1 failure' }).locator(':scope > button').click()
  await expect(page.getByTestId('agent-step').filter({ hasText: 'Validation needs attention.' })).toHaveCount(1)
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:disconnect' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('failed')
  await page.goto(`/chats/${chat.id}`)
  await expect(page.getByRole('alert')).toContainText('Codex')
  await expect(page.getByRole('alert')).not.toContainText('A successful first reply')
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    await page.setViewportSize({ width: 390, height: 844 })
    await expectSingleScroll(page)
    await page.screenshot({ path: test.info().outputPath(`chat-failure-${colorScheme}.png`), animations: 'disabled' })
  }

  await page.getByRole('button', { name: 'Dismiss error' }).click()
  await expect(page.getByRole('alert')).toHaveCount(0)
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('Codex')
  const run = workspace.service.chats.detail(chat.id).run!
  workspace.service.store.updateRun(run.id, { error: null, summary: 'An old **successful** reply' })
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('The response stopped before finishing')
  await expect(page.getByRole('alert')).not.toContainText('successful')
})

test('starts project chats, steers, edits the queue and preserves a compact mobile composer', async ({ page, workspace }) => {
  initializeRepository(workspace.projectPath)
  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  await page.goto('/projects')
  await page.getByRole('link', { name: 'Start chat' }).first().click()
  await expect(page.getByRole('combobox', { name: 'Chat agent' })).toHaveValue('Main agent')
  await expect(page.getByRole('combobox', { name: 'Chat project' })).toHaveValue('Design system')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('chat-new-desktop-light.png'), animations: 'disabled' })
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Review our component architecture. fixture:chat-hang')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-f0-9-]+$/)
  await expect(page.getByText('Working', { exact: true }).first()).toBeVisible()
  await expect(page.getByTestId('agent-actions').filter({ hasText: 'Followed the run' })).toHaveCount(0)
  await expect(page.locator('.activity-message').filter({ hasText: 'Review our component architecture' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Add a regression test for the composer')
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.getByRole('button', { name: /^Queued message options/ }).first().click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Add a regression test for keyboard navigation')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Focus on accessibility first.')
  await page.getByRole('button', { name: 'Steer now' }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Focus on accessibility first.' }).first()).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Consider the mobile experience too')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Queue', exact: true })).toBeInViewport()
  await page.screenshot({ path: test.info().outputPath('chat-mobile-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ path: test.info().outputPath('chat-mobile-light.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 600 })
  await expect(page.getByRole('button', { name: 'Queue', exact: true })).toBeInViewport()
  await page.emulateMedia({ colorScheme: 'dark' })
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Steer now' })).toBeInViewport()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.screenshot({ path: test.info().outputPath('chat-desktop-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ path: test.info().outputPath('chat-desktop-light.png'), animations: 'disabled' })
  await page.reload()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Consider the mobile experience too')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('finish now')
  await page.getByRole('button', { name: 'Steer now' }).click()
  await expect.poll(() => workspace.service.chats.detail(workspace.service.store.list('chats')[0].id).messages.filter(message => message.status !== 'delivered').length).toBe(0)
  await expectChatReady(page)
  await expect(page.locator('.activity-message').filter({ hasText: 'Add a regression test for keyboard navigation' }).last()).toBeVisible()
  await page.goto('/agents')
  await page.locator('article').filter({ has: page.getByRole('heading', { name: 'Release engineer', exact: true }) }).getByRole('link', { name: 'Start chat' }).click()
  await expect(page.getByRole('combobox', { name: 'Chat agent' })).toHaveValue('Release engineer')
})

test('answers in-flight questions with choices or free text on desktop and mobile', async ({ page, workspace }) => {
  // Earlier chat journeys share this worker's request budget. Start this
  // independent flow with a fresh limiter while preserving the seeded data.
  await workspace.restart()
  initializeRepository(workspace.projectPath)
  await page.goto('/tasks')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  await page.goto('/chats')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Plan the navigation refresh. fixture:question')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Answer pending questions' })).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.emulateMedia({ colorScheme: 'light' })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('questions-chat-desktop.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Answer', exact: true }).click()
  await expect(page.getByText('Agent keeps working', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Send answer', exact: true })).toBeDisabled()
  await expect(page.getByRole('radio').first()).not.toBeChecked()
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('questions-desktop-light.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: test.info().outputPath('questions-desktop-dark.png'), animations: 'disabled' })
  await page.getByRole('radio', { name: /Gradual rollout/ }).check()
  await expect(page.getByRole('button', { name: 'Send answer', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Answer later' }).click()
  await page.reload()
  await expect(page.getByRole('button', { name: 'Answer pending questions' })).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('button', { name: 'Answer', exact: true }).click()
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('questions-mobile-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.getByRole('textbox', { name: 'Or write your own answer' }).fill('Start with the chat, keeping keyboard navigation intact.')
  await page.screenshot({ path: test.info().outputPath('questions-mobile-light.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 600 })
  await expectSingleScroll(page)
  await page.getByRole('button', { name: 'Send answer', exact: true }).click()
  await expect(page.getByRole('dialog')).not.toBeVisible()
  await expect(page.getByRole('button', { name: 'Answer pending questions' })).not.toBeVisible()
  await expectChatReady(page)
  const chatId = page.url().split('/').at(-1)!
  expect(workspace.service.questions.list(chatId)[0].status).toBe('answered')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('button', { name: 'Question notifications' }).click()
  await expect(page.getByRole('heading', { name: 'Question notifications' })).toBeVisible()
  await page.screenshot({ path: test.info().outputPath('notifications-mobile.png'), animations: 'disabled' })
})

test('installation push registration is removed in favor of the official account', async ({ page, workspace }) => {
  await workspace.restart()
  await page.goto('/chats')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.getByRole('button', { name: 'Question notifications' }).click()
  await expect(page.getByText('Sign in to your Leo account to manage push notifications on this device.')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Enable on this device' })).toHaveCount(0)
  expect((await page.request.get(`${workspace.url}/api/notifications`)).status()).toBe(404)
  expect(workspace.service.store.keys('push-device:')).toHaveLength(0)
})

test('long words and URLs wrap inside messages without widening the conversation', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  const url = `https://example.test/files/${'a1b2c3d4'.repeat(28)}.pdf`
  const word = 'UnbrokenWord'.repeat(30)
  const text = `Here is the document:\n${url}\n\nReference: ${word}`
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  const run = workspace.service.chats.detail(chat.id).run!
  const reply = `The document is ready: [${url}](${url}).\n\nReference: \`${word}\`.\n\n\`\`\`text\n${word}\n\`\`\``
  workspace.service.store.event(run.id, 'item.completed', reply, { item: { type: 'agent_message', text: reply } })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  const user = page.locator('.activity-message').filter({ has: page.locator('p', { hasText: 'Here is the document:' }) }).first()
  await expect(user).toContainText(text)
  const scroller = page.getByRole('region', { name: 'Activity output' })

  async function fits() {
    await expect.poll(() => scroller.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1)
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    const overflow = await user.locator('p').first().evaluate((el) => {
      const bounds = el.getBoundingClientRect()
      const range = document.createRange()
      range.selectNodeContents(el)
      return [...range.getClientRects()].some(rect => rect.left < bounds.left - 1 || rect.right > bounds.right + 1)
    })
    expect(overflow).toBe(false)
    await expectSingleScroll(page)
  }

  for (const width of [390, 320, 1440]) {
    await page.setViewportSize({ width, height: width === 1440 ? 960 : 844 })
    await fits()
  }

  await page.setViewportSize({ width: 390, height: 844 })
  await user.scrollIntoViewIfNeeded()
  await page.screenshot({ path: test.info().outputPath('long-message-mobile.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('button', { name: 'Open activity fullscreen', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Fullscreen activity' })).toBeVisible()
  await fits()
  await page.getByRole('button', { name: 'Exit fullscreen', exact: true }).click()
  await page.reload()
  await expect(user).toContainText(text)
  await fits()
  await workspace.restart()
})

test('conversation trash stays secondary and restores a deleted conversation', async ({ page, workspace }, testInfo) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...chat, title: 'Conversation to recover' })
  await page.goto('/chats')
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.getByRole('button', { name: 'Conversations', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: /Conversations/ })
  await expect(dialog.getByLabel('Conversation view')).toHaveValue('active')
  await page.screenshot({ path: testInfo.outputPath('active-conversations.png') })
  await dialog.getByLabel('Actions for Conversation to recover').click()
  await dialog.getByRole('button', { name: 'Delete', exact: true }).click()
  await expect(dialog.getByRole('link', { name: /Conversation to recover/ })).toHaveCount(0)
  await expect(dialog.getByRole('option', { name: 'Archives', exact: true })).toHaveCount(0)
  await dialog.getByLabel('Conversation view').selectOption('trash')
  await dialog.getByRole('link', { name: /Conversation to recover/ }).click()
  await expect(page.getByText('This conversation is in the trash.')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('conversation-trash.png') })
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveCount(0)
  await page.getByRole('button', { name: 'Restore conversation', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Conversations', exact: true }).click()
  await expect(dialog.getByLabel('Conversation view')).toHaveValue('active')
  await expect(dialog.getByRole('link', { name: /Conversation to recover/ })).toBeVisible()
})

test('deletion from another client removes the open transcript', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const message = 'Private conversation removed remotely'
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: message })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  await page.goto(`/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.activity-message').getByText(message, { exact: true }).first()).toBeVisible({ timeout: 30000 })
  await workspace.api(`/api/chats/${chat.id}`, 'DELETE', {})
  await expect(page.getByText('This conversation is in the trash.')).toBeVisible()
  await expect(page.locator('.activity-message').getByText(message, { exact: true })).toHaveCount(0)
  await page.reload()
  await expect(page.getByText('This conversation is in the trash.')).toBeVisible()
  await expect(page.locator('.activity-message').getByText(message, { exact: true })).toHaveCount(0)
})
