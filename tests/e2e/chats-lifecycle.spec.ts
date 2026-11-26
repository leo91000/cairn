import type { Route } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  closeChatDetails,
  expect,
  expectChatReady,
  expectSingleScroll,
  headerStatus,
  initializeRepository,
  openChatDetails,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('conversation navigation uses desktop search and the phone switcher without losing drafts', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await workspace.restart()
  const first = await workspace.api('/api/chats', 'POST', {})
  const second = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...first, title: 'Simplify the chat navigation', updatedAt: Date.now() })
  workspace.service.store.put('chats', { ...second, title: 'Review the release notes', updatedAt: Date.now() - 86400000 })
  await page.goto(workspacePath(`/chats/${first.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Simplify the chat navigation', exact: true })).toBeVisible()
  const message = page.getByRole('textbox', { name: 'Message', exact: true })
  await message.fill('Keep this unsent draft')
  const trigger = page.getByRole('button', { name: 'Conversations', exact: true })
  const dialog = page.getByRole('dialog', { name: /Conversations/ })
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1440, 800]) {
      await page.setViewportSize({ width, height: 900 })
      await expect(trigger).toBeHidden()
      await expect(page.getByRole('heading', { name: 'Simplify the chat navigation', exact: true })).toBeVisible()
      const shortcut = width === 1440 ? 'Control+k' : 'Meta+k'
      const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
      await page.keyboard.press(shortcut)
      const search = palette.getByRole('combobox', { name: 'Search', exact: true })
      await expect(search).toBeFocused()
      await search.fill('Review the release notes')
      await expect(palette.getByRole('option', { name: /^Review the release notes/ })).toBeVisible()
      await search.press('Enter')
      await expect(page).toHaveURL(workspacePath(`/chats/${second.slug}`))
      await expect(message).toHaveValue('')
      await page.keyboard.press(shortcut)
      await search.fill('Simplify the chat navigation')
      await expect(palette.getByRole('option', { name: /^Simplify the chat navigation/ })).toBeVisible()
      await search.press('Enter')
      await expect(page).toHaveURL(workspacePath(`/chats/${first.slug}`))
      await expect(message).toHaveValue('Keep this unsent draft')
    }

    for (const width of [390, 320]) {
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

  // Resizing an open phone list restores focus to a visible desktop control.
  await page.setViewportSize({ width: 390, height: 844 })
  await trigger.click()
  await expect(dialog).toBeVisible()
  await page.setViewportSize({ width: 800, height: 900 })
  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Chat details', exact: true })).toBeFocused()

  await page.setViewportSize({ width: 320, height: 600 })
  await trigger.click()
  const search = page.getByRole('searchbox', { name: 'Search conversations' })
  await search.fill('nothing-matches-this')
  await expect(dialog.getByRole('status')).toHaveText('No conversations match your search.')
  await search.fill('release notes')
  await expect(dialog.getByRole('link')).toHaveCount(1)
  await dialog.getByRole('link', { name: /Review the release notes/ }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${second.slug}`))
  await expect(dialog).toHaveCount(0)
  await expect(message).toHaveValue('')
  await trigger.click()
  await dialog.getByRole('link', { name: /Simplify the chat navigation/ }).click()
  await expect(message).toHaveValue('Keep this unsent draft')
  await trigger.click()
  await dialog.getByRole('button', { name: 'New conversation', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/chats'))
  await expect(dialog).toHaveCount(0)
  await expect(message).toHaveValue('')
  await workspace.restart()
})

test('workspace shortcuts open search while the initial refresh is pending', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await useRelayForHttpMocks(page)
  let refreshRequests = 0
  let releaseRefresh!: () => void
  const refreshPending = new Promise<void>((resolve) => {
    releaseRefresh = resolve
  })
  await page.route('**/api/mcps', async (route) => {
    refreshRequests++
    await refreshPending
    await route.continue()
  })

  try {
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const message = page.getByRole('textbox', { name: 'Message', exact: true })
    await message.fill('Keep this draft during startup')
    await expect.poll(() => refreshRequests).toBeGreaterThan(0)
    const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
    const search = palette.getByRole('combobox', { name: 'Search', exact: true })

    for (const shortcut of ['Control+k', 'Meta+k']) {
      await page.keyboard.press(shortcut)
      await expect(search).toBeFocused()
      await search.press('Escape')
      await expect(palette).toHaveCount(0)
      await expect(message).toBeFocused()
      await expect(message).toHaveValue('Keep this draft during startup')
    }
  }
  finally {
    releaseRefresh()
  }
})

test('workspace search opens legacy trash and restores a conversation without losing another draft', async ({ page, workspace }) => {
  const active = await workspace.api('/api/chats', 'POST', {})
  const trashed = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...active, title: 'Keep the current work draft' })
  workspace.service.store.put('chats', {
    ...trashed,
    title: 'Legacy recoverable conversation',
    lifecycle: 'trash',
    trashedAt: Date.now() - 86400000,
    purgeAt: Date.now() + 86400000,
  })
  await page.goto(workspacePath(`/chats/${active.id}`))
  await authenticateWorkspace(page)
  const message = page.getByRole('textbox', { name: 'Message', exact: true })
  await message.fill('Keep this draft while visiting the trash')
  const trash = page.getByRole('dialog', { name: /Conversations/ })
  for (const fromFil of [false, true]) {
    if (fromFil) {
      await page.getByRole('navigation', { name: 'Workspace navigation', exact: true }).getByRole('link', { name: 'Fil', exact: true }).click()
      await expect(page).toHaveURL(workspacePath('/'))
    }

    await page.keyboard.press('Control+k')
    const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
    await palette.getByRole('combobox', { name: 'Search', exact: true }).fill('Conversation trash')
    await palette.getByRole('option', { name: 'Conversation trash', exact: true }).click()
    await expect(trash.getByRole('link', { name: /^Legacy recoverable conversation/ })).toBeVisible()
    await expect(trash.getByRole('link', { name: /^Keep the current work draft/ })).toHaveCount(0)
    if (!fromFil) {
      await trash.getByRole('button', { name: 'Close conversations', exact: true }).click()
      await expect(message).toHaveValue('Keep this draft while visiting the trash')
    }
  }

  await trash.getByRole('link', { name: /^Legacy recoverable conversation/ }).click()
  await page.getByRole('button', { name: 'Restore conversation', exact: true }).click()
  await expect(message).toBeVisible()
  expect((await workspace.api(`/api/chats/${trashed.id}`)).lifecycle).toBe('active')
  await page.keyboard.press('Control+k')
  const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
  const search = palette.getByRole('combobox', { name: 'Search', exact: true })
  await search.fill('Keep the current work draft')
  await palette.getByRole('option', { name: /^Keep the current work draft/ }).click()
  await expect(message).toHaveValue('Keep this draft while visiting the trash')
})

test('compact mobile toolbar keeps details and workspace controls accessible', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the mobile navigation and keep the conversation easy to find.' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
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
      const workspaceTop = (await page.locator('.shell').boundingBox())!.y
      expect(Math.abs(header!.y - workspaceTop)).toBeLessThanOrEqual(1)
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
  // The ⋯ menu acts on this conversation only: workspace search, navigation and appearance live elsewhere.
  for (const elsewhere of ['New chat', 'Search conversations', 'Search workspace', 'Workspace navigation', 'Follow latest output'])
    await expect(panel.getByRole('button', { name: elsewhere, exact: true })).toHaveCount(0)
  await expect(panel.getByRole('button', { name: /Appearance:/ })).toHaveCount(0)
  await panel.getByRole('button', { name: 'Open activity fullscreen', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Fullscreen activity' })).toBeVisible()
  await page.getByRole('button', { name: 'Exit fullscreen', exact: true }).click()
  await expect(details).toBeFocused()
  await expect(message).toHaveValue('Keep my draft while browsing details')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeVisible()
  // Desktop keeps one quiet header row; fullscreen moves into the ⋯ dropdown.
  await expect(page.locator('.activity-toolbar')).not.toBeVisible()
  await expect(page.locator('.chat-header').getByRole('button', { name: 'Open activity fullscreen' })).toHaveCount(0)
  await details.click()
  const menu = page.getByRole('menu', { name: 'Chat details', exact: true })
  await expect(menu.locator('..').getByText('Ready', { exact: true })).toBeVisible()
  // The dropdown shows the full title the header may truncate.
  const shownTitle = (await page.locator('.chat-header h1:not(.sr-only)').textContent())!.trim()
  await expect(menu.locator('..').locator('p').first()).toHaveText(shownTitle)
  // Screen readers hear the agent, its status and context with the menu, not only its actions.
  await expect(menu).toHaveAccessibleDescription(/Main agent.*Ready.*Context/)
  await expect(menu.getByRole('group', { name: 'Advanced', exact: true }).getByRole('menuitem', { name: 'Connection', exact: true })).toBeVisible()
  await expect(menu.getByRole('menuitem').first()).toBeFocused()
  await expect(menu.getByRole('menuitem', { name: 'Open activity fullscreen', exact: true })).toBeVisible()
  await page.keyboard.press('End')
  await expect(menu.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeFocused()
  // Clicking the summary keeps the menu's keys working, from either end.
  await menu.locator('..').getByText('Ready', { exact: true }).click()
  await page.keyboard.press('ArrowDown')
  await expect(menu.getByRole('menuitem').first()).toBeFocused()
  await menu.locator('..').getByText('Ready', { exact: true }).click()
  await page.keyboard.press('ArrowUp')
  await expect(menu.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeFocused()
  await page.keyboard.press('Escape')
  await expect(menu).toBeHidden()
  await expect(details).toBeFocused()
  await details.click()
  await menu.getByRole('menuitem', { name: 'Open activity fullscreen', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Fullscreen activity' })).toBeVisible()
  await page.getByRole('button', { name: 'Exit fullscreen', exact: true }).click()
  await expect(details).toBeFocused()
  // Closing after the window narrowed to a phone returns focus to the ⋯ control shown there.
  await details.click()
  await menu.getByRole('menuitem', { name: 'Connection', exact: true }).click()
  const connectionSheet = page.getByRole('dialog', { name: 'Connection', exact: true })
  await expect(connectionSheet).toBeVisible()
  await page.setViewportSize({ width: 390, height: 844 })
  await connectionSheet.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await expect(connectionSheet).toHaveCount(0)
  await expect(details).toBeFocused()
  await expect(details).toBeInViewport()
  await page.setViewportSize({ width: 1440, height: 1000 })
  // A short window scrolls the dropdown, so its last actions stay reachable.
  await page.setViewportSize({ width: 844, height: 390 })
  await details.click()
  await page.keyboard.press('End')
  await expect(menu.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeFocused()
  await expect(menu.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeInViewport({ ratio: 1 })
  await page.keyboard.press('Escape')
  // Opening with ArrowUp focuses the last action and scrolls it into view.
  await page.keyboard.press('ArrowUp')
  await expect(menu.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeFocused()
  await expect(menu.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeInViewport({ ratio: 1 })
  await page.keyboard.press('Escape')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expect(message).toHaveValue('Keep my draft while browsing details')
  await page.setViewportSize({ width: 390, height: 844 })
  // New conversations start from the conversation list.
  await expect(page.getByRole('link', { name: 'New chat', exact: true })).toBeHidden()
  await page.getByRole('button', { name: 'Conversations', exact: true }).click()
  await page.getByRole('button', { name: 'New conversation', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/chats'))
  await details.click()
  await expect(panel.getByRole('button', { name: 'Connection', exact: true })).toBeVisible()
  await expect(panel.getByRole('button', { name: 'Open activity fullscreen' })).toHaveCount(0)
  await page.keyboard.press('Escape')
  await expectSingleScroll(page)
  // This standalone journey must not consume later chats' request budget.
  await workspace.restart()
})

test('phone headers carry the installation, its route and the conversation connection', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Check the phone header.' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(workspacePath('/'))
  await authenticateWorkspace(page)

  // The Fil header replaces the global installation bar on phones.
  const banner = page.getByRole('banner', { name: 'Current installation', exact: true })
  await expect(banner).toBeHidden()
  const installation = page.getByRole('group', { name: 'Installation', exact: true })
  await expect(installation.getByRole('status', { name: 'Installation availability', exact: true })).toHaveText('Online')
  await expect(installation.getByRole('status', { name: 'Connection route', exact: true })).toHaveAttribute('data-transport-route', /^(direct|relay)$/)
  await expect(installation.getByRole('button', { name: 'Search workspace', exact: true })).toBeVisible()
  await expect(installation.getByRole('button', { name: 'Account', exact: true })).toBeVisible()
  await installation.getByRole('button', { name: 'Browser workspace', exact: true }).click()
  const connection = page.getByRole('dialog', { name: 'Connection', exact: true })
  await expect(connection).toContainText('Owner')
  await expect(connection).toContainText(/Direct|Relais/)
  await expect(connection.getByText('Agent · project', { exact: true })).toHaveCount(0)
  await connection.getByRole('button', { name: 'Installation settings', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/settings/installation'))

  // A conversation keeps one row: agent, title and its route, files and the ⋯ menu. The installation names the route.
  await page.goto(workspacePath(`/chats/${chat.id}`))
  const header = page.locator('.chat-header')
  await expect(header.getByRole('button', { name: 'Conversations', exact: true }).getByTitle(/^Browser workspace · /)).toBeVisible()
  await expect(header.getByRole('button', { name: 'Conversations', exact: true })).toHaveAccessibleDescription(/^Main agent · Latest harness: Codex · Browser workspace · Online · (Direct|Relais)$/)
  expect(Math.round((await header.boundingBox())!.height)).toBeLessThanOrEqual(60)
  await expect(header.getByRole('button', { name: /^Files: \d+$/ })).toBeVisible()
  await expect(page.getByRole('link', { name: 'New chat', exact: true })).toBeHidden()
  await expectSingleScroll(page)
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('dialog', { name: 'Chat details', exact: true }).getByRole('button', { name: 'Connection', exact: true }).click()
  await expect(connection.getByText('Agent · project', { exact: true })).toBeVisible()
  await connection.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await expect(connection).toHaveCount(0)

  // Desktop starts conversations from the navigation rail, and keeps its installation bar on the Fil.
  await page.setViewportSize({ width: 1280, height: 800 })
  await expect(header.getByRole('heading', { name: 'Check the phone header.', exact: true })).toBeVisible()
  await expect(header.getByRole('img', { name: 'Latest harness: Codex', exact: true })).toBeVisible()
  await expect(page.getByRole('link', { name: 'New chat', exact: true })).toHaveCount(0)
  await expect(header.getByRole('button', { name: /^Files: / })).toBeHidden()
  await page.goto(workspacePath('/'))
  await expect(page.locator('.installation-header')).toHaveCount(1)
  await expect(installation).toBeHidden()
  await expect(banner).toBeVisible()

  // The phone account menu still reaches sign-out. Not clicked: the worker's session is shared with later tests.
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(installation).toBeVisible()
  const account = installation.getByRole('button', { name: 'Account', exact: true })
  await account.click()
  const signOut = page.getByRole('menuitem', { name: 'Sign out', exact: true })
  await expect(signOut).toBeInViewport()
  await expect(signOut).toBeEnabled()
  await page.keyboard.press('Escape')
  await expect(signOut).toBeHidden()
  await expect(account).toBeFocused()
})

test('ordinary sends and steering stay in the transcript while only follow-ups queue', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  test.setTimeout(90000)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
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
  await page.getByRole('button', { name: 'Send after this turn', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await expect(userMessages.filter({ hasText: 'A follow-up for the next reply' })).toHaveCount(0)
  const steer = 'Focus on accessibility. fixture:slow-delivery'
  await composer.fill(steer)
  await page.getByRole('button', { name: 'Send now', exact: true }).click()
  await expect(userMessages.filter({ hasText: steer }).getByRole('status')).toHaveText('Sending to agent…')
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.screenshot({ path: test.info().outputPath('steer-and-real-queue-mobile.png'), animations: 'disabled' })
  await expect(userMessages.filter({ hasText: steer }).getByRole('status')).toHaveCount(0, { timeout: 15000 })
  await expect(userMessages.filter({ hasText: steer })).toHaveCount(1)
  // Stopping returns what the turn never delivered to the composer; nothing waits to be resumed.
  await page.getByRole('button', { name: 'Stop response', exact: true }).click()
  await expect(composer).toHaveValue('A follow-up for the next reply')
  await expect(page.getByTestId('conversation-queue')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toHaveCount(0)
  await page.screenshot({ path: test.info().outputPath('returned-message-mobile.png'), animations: 'disabled' })
  await composer.fill('Send this after stopping')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(userMessages.filter({ hasText: 'Send this after stopping' })).toHaveCount(1)
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(false)
  await workspace.restart()
})

test('a rejected ordinary send preserves its draft and removes the optimistic message', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await workspace.restart()
  const chat = await workspace.api('/api/chats', 'POST', {})
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
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
      'Changes pushed to **main** · https://github.com/leo91000/cairn/commit/2b62b45',
      '**Local validation passed**\n\n73 tests · lint · debug & release builds',
      '**Android CI passed** · https://github.com/leo91000/cairn/actions/runs/35315999841',
      `Published APK: ${artifactPath}`,
      '[Review notes](https://example.test/review) · [Unsafe link](javascript:alert(1))',
    ],
    reportedAt: Date.now(),
    messageId,
  }
  workspace.service.store.updateRun(run.id, { outcome })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
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
  await expect(panel.getByRole('link', { name: 'View commit' })).toHaveAttribute('href', 'https://github.com/leo91000/cairn/commit/2b62b45')
  await expect(panel.getByRole('link', { name: 'View workflow' })).toHaveAttribute('target', '_blank')
  await expect(panel.getByRole('link', { name: 'View workflow' })).toHaveAttribute('rel', 'noopener noreferrer')
  await expect(panel.getByRole('link', { name: 'Review notes' })).toHaveAttribute('href', 'https://example.test/review')
  await expect(panel.getByRole('link', { name: 'View file' })).toHaveAttribute('href', `/api/installations/${workspace.installationId}${artifactPath}`)
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
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByRole('note').filter({ hasText: 'Validation needs attention.' })).toBeVisible()
  await expect(page.getByRole('note').filter({ hasText: 'Interrupted' })).toBeVisible()
  await expect(page.getByTestId('agent-actions').filter({ hasText: 'Followed the run' })).toHaveCount(0)
  for (const width of [1440, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await expectSingleScroll(page)
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeInViewport()
  }

  // Diagnostic run activity still has the complete lifecycle records.
  await page.goto(workspacePath(`/runs/${firstRun.id}`))
  await page.getByTestId('agent-actions').filter({ hasText: '1 failure' }).locator(':scope > button').click()
  await expect(page.getByTestId('agent-step').filter({ hasText: 'Validation needs attention.' })).toHaveCount(1)
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:disconnect' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('failed')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  // The failure card gives the reason once; the banner above the composer does not repeat it.
  await expect(page.getByRole('note').filter({ hasText: 'Codex' })).toHaveCount(1)
  // The header names the failure in the error tone: under the title on phones, beside it on desktop.
  await expect(headerStatus(page, /^Failed$/).locator('..')).toHaveClass(/text-coral/)
  await page.setViewportSize({ width: 1280, height: 800 })
  await expect(headerStatus(page, /^Failed$/)).toHaveClass(/text-coral/)
  // Opening a failed conversation is not news: the live region waits for a change.
  await page.waitForTimeout(1500)
  await expect(page.locator('.chat-header .sr-only[role="status"]')).toHaveText('')
  await expect(page.getByRole('alert')).toHaveCount(0)
  for (const colorScheme of ['dark', 'light'] as const) {
    await page.emulateMedia({ colorScheme })
    await page.setViewportSize({ width: 390, height: 844 })
    await expectSingleScroll(page)
    await page.screenshot({ path: test.info().outputPath(`chat-failure-${colorScheme}.png`), animations: 'disabled' })
  }

  const run = workspace.service.chats.detail(chat.id).run!
  workspace.service.store.updateRun(run.id, { error: null, summary: 'An old **successful** reply' })
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('The response stopped before finishing')
  await expect(page.getByRole('alert')).not.toContainText('successful')
  await page.getByRole('button', { name: 'Dismiss error' }).click()
  await expect(page.getByRole('alert')).toHaveCount(0)
  await page.reload()
  await expect(page.getByRole('alert')).toContainText('The response stopped before finishing')
})

test('withdraws an obsolete question without sending an answer on desktop and mobile', async ({ page, workspace }) => {
  await workspace.restart()
  for (const width of [1440, 390]) {
    const chat = await workspace.api('/api/chats', 'POST', {})
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:cairn-question' })
    await page.setViewportSize({ width, height: 844 })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    await expect(page.getByRole('button', { name: 'Answer pending questions' })).toBeVisible()
    await page.getByRole('button', { name: 'Answer', exact: true }).click()
    await expect(page.getByRole('button', { name: 'Send answer', exact: true })).toBeDisabled()
    await page.getByRole('button', { name: 'No longer needed', exact: true }).click()
    await expect(page.getByRole('dialog')).not.toBeVisible()
    await expect(page.getByRole('button', { name: 'Answer pending questions' })).not.toBeVisible()
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    expect(detail.questions[0].status).toBe('cancelled')
    expect(detail.pendingQuestions).toBe(0)
    expect(detail.messages).toHaveLength(0)
    const userEvents = workspace.service.store.events(detail.run.id, 0, 100).filter(event => event.type === 'chat.user')
    expect(userEvents).toHaveLength(1)
    await page.reload()
    await expect(page.getByRole('button', { name: 'Answer pending questions' })).not.toBeVisible()
  }
})

test('answers in-flight questions with choices or free text on desktop and mobile', async ({ page, workspace }) => {
  // Earlier chat journeys share this worker's request budget. Start this
  // independent flow with a fresh limiter while preserving the seeded data.
  await workspace.restart()
  initializeRepository(workspace.projectPath)
  await page.goto(workspacePath('/tasks'))
  await authenticateWorkspace(page)
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  await page.goto(workspacePath('/chats'))
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Plan the navigation refresh. fixture:cairn-question')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Answer pending questions' })).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.emulateMedia({ colorScheme: 'light' })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('questions-chat-desktop.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Answer', exact: true }).click()
  await expect(page.getByText('Continue the conversation', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Send answer', exact: true })).toBeDisabled()
  await expect(page.getByRole('checkbox').first()).not.toBeChecked()
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('questions-desktop-light.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: test.info().outputPath('questions-desktop-dark.png'), animations: 'disabled' })
  await page.getByRole('checkbox', { name: /Chat view/ }).check()
  await page.getByRole('checkbox', { name: /Android/ }).check()
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
  await page.getByRole('textbox', { name: 'Comment or your own answer (optional with choices)' }).fill('Start with the chat, keeping keyboard navigation intact.')
  await page.screenshot({ path: test.info().outputPath('questions-mobile-light.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 600 })
  await expectSingleScroll(page)
  await page.getByRole('button', { name: 'Send answer', exact: true }).click()
  await expect(page.getByRole('dialog')).not.toBeVisible()
  await expect(page.getByRole('button', { name: 'Answer pending questions' })).not.toBeVisible()
  const chatKey = new URL(page.url()).pathname.split('/').at(-1)!
  const chatId = (await workspace.api(`/api/chats/${chatKey}`)).id
  // Submission queues the answer; Ready can still describe the preceding turn.
  await expect.poll(() => workspace.service.questions.list(chatId)[0].status).toBe('answered')
  await expectChatReady(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(workspacePath('/settings/account'))
  await expect(page.getByRole('heading', { name: 'Question notifications' })).toBeVisible()
  await page.screenshot({ path: test.info().outputPath('notifications-mobile.png'), animations: 'disabled' })
})

test('installation push registration is removed in favor of the beacon account', async ({ page, workspace }) => {
  await workspace.restart()
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  // Notifications belong to the account, not to a conversation: its menu no longer links them.
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  // A conversation not created yet only offers its connection.
  await expect(page.getByRole('menu', { name: 'Chat details', exact: true }).getByRole('menuitem')).toHaveText(['Connection'])
  await page.keyboard.press('Escape')
  await page.goto(workspacePath('/settings/account'))
  await expect(page.getByText('Know when your agent needs your input, across all your installations, even with the app closed. Question content stays private.')).toBeVisible()
  await expect(page.getByText('Sign in to your Cairn account to manage push notifications on this device.')).toHaveCount(0)
  expect((await page.request.get(`${workspace.url}/api/installations/${workspace.installationId}/api/notifications`)).status()).toBe(404)
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
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
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

test('a renamed conversation keeps its title and Cairn stops naming it', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Draft the release notes for the next version' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const details = page.getByRole('button', { name: 'Chat details', exact: true })
  const dialog = page.getByRole('dialog', { name: 'Rename conversation', exact: true })
  const title = dialog.getByRole('textbox', { name: 'Title', exact: true })

  // The dropdown offers renaming and says what it changes.
  let { action } = await openChatDetails(page)
  await action('Rename conversation').click()
  await expect(dialog).toContainText('Cairn stops naming this conversation automatically')
  await expect(title).toHaveValue('Draft the release notes for the next version')
  await expect(title).toBeFocused()
  await title.fill('   ')
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeDisabled()
  // Counted in characters like the installation: emoji count once, and too long is explained.
  await title.fill('🚀'.repeat(91))
  await expect(dialog).toContainText('Use at most 90 characters (91 now).')
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeDisabled()
  await title.fill('🚀'.repeat(90))
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeEnabled()
  await title.fill('  Release notes  ')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(details).toBeFocused()
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Release notes', exact: true })).toBeVisible()
  const fil = page.getByRole('complementary', { name: 'Fil' })
  await fil.getByRole('tab', { name: /^Tout/ }).click()
  await expect(fil.locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText('Release notes')
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).title).toBe('Release notes')

  // A message and its turn while the dialog is open leave the conversation renamable at the
  // revision it now has.
  ;({ action } = await openChatDetails(page))
  await action('Rename conversation').click()
  await expect(title).toHaveValue('Release notes')
  const before = (await workspace.api(`/api/chats/${chat.id}`)).revision
  const followUp = randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: followUp, text: 'Add the migration notes too' })
  // The conversation's run is reused, so its turn is done once it took the message and succeeded.
  await expect.poll(() => {
    const current = workspace.service.chats.detail(chat.id)
    return current.messages.some(message => message.id === followUp && message.status === 'delivered') && current.run?.status === 'succeeded'
  }).toBe(true)
  expect((await workspace.api(`/api/chats/${chat.id}`)).revision).not.toBe(before)
  await title.fill('Release notes 0.52')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Release notes 0.52', exact: true })).toBeVisible()

  // Another client renames it while the dialog is open: the live title shows, the field keeps what
  // the person typed, and the dialog names the new title before saving replaces it.
  ;({ action } = await openChatDetails(page))
  await action('Rename conversation').click()
  await expect(title).toHaveValue('Release notes 0.52')
  // The finished turn may still update the conversation, so the other client retries on a stale revision.
  await expect(async () => {
    const current = await workspace.api(`/api/chats/${chat.id}`)
    await workspace.api(`/api/chats/${chat.id}`, 'PUT', { revision: current.revision, title: 'Renamed elsewhere' })
  }).toPass()
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Renamed elsewhere', exact: true })).toBeVisible()
  await expect(dialog.getByRole('alert')).toHaveText('The title changed to “Renamed elsewhere” meanwhile; saving replaces it.')
  // Save follows the current title: the untouched field would restore the earlier one, and the
  // new title itself is nothing to save.
  await expect(title).toHaveValue('Release notes 0.52')
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeEnabled()
  await title.fill('Renamed elsewhere')
  await expect(dialog.getByRole('button', { name: 'Save', exact: true })).toBeDisabled()
  await title.fill('Racing title')
  expect((await workspace.api(`/api/chats/${chat.id}`)).title).toBe('Renamed elsewhere')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Racing title', exact: true })).toBeVisible()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).title).toBe('Racing title')

  // A title changed while saving reads the conversation is named with focus back in the field, even
  // when the live conversation shows it before the read returns, and the next save replaces it.
  ;({ action } = await openChatDetails(page))
  await action('Rename conversation').click()
  await expect(title).toHaveValue('Racing title')
  await expect(dialog.getByRole('alert')).toHaveCount(0)
  const renamePath = `**/chats/${chat.id}`
  await title.fill('Found title')
  let renameDuringRead = true
  await page.route(renamePath, async (route) => {
    if (route.request().method() === 'GET' && renameDuringRead) {
      renameDuringRead = false
      const current = await workspace.api(`/api/chats/${chat.id}`)
      await workspace.api(`/api/chats/${chat.id}`, 'PUT', { revision: current.revision, title: 'Automatic title' })
      await page.locator('.chat-header').getByRole('heading', { name: 'Automatic title', exact: true }).waitFor()
    }

    return route.continue()
  })
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog.getByRole('alert')).toHaveText('The title changed to “Automatic title” meanwhile; saving replaces it.')
  await expect(title).toBeFocused()
  await expect(title).toHaveValue('Found title')
  await title.press('Enter')
  await expect(dialog).toHaveCount(0)
  await page.unroute(renamePath)
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Found title', exact: true })).toBeVisible()

  // A revision that moves between reading and renaming is read once more, unseen.
  let refusals = 1
  let renames = 0
  await page.route(renamePath, (route: Route) => {
    if (route.request().method() !== 'PUT')
      return route.continue()
    renames++
    if (refusals === 0)
      return route.continue()
    refusals--
    return route.fulfill({ status: 409, json: { error: 'This conversation changed. Reload before renaming.' } })
  })
  ;({ action } = await openChatDetails(page))
  await action('Rename conversation').click()
  await expect(title).toHaveValue('Found title')
  await title.fill('Retried title')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Retried title', exact: true })).toBeVisible()
  expect(renames).toBe(2)

  // A rename refused again is reported and the field takes focus back, keeping what the person typed.
  refusals = 2
  renames = 0
  ;({ action } = await openChatDetails(page))
  await action('Rename conversation').click()
  await expect(title).toHaveValue('Retried title')
  await expect(dialog.getByRole('alert')).toHaveCount(0)
  await title.fill('Final title')
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog.getByRole('alert')).toContainText('This conversation changed. Reload before renaming.')
  expect(renames).toBe(2)
  await expect(title).toBeFocused()
  await page.keyboard.press('End')
  await page.keyboard.type('!')
  await expect(title).toHaveValue('Final title!')
  await page.unroute(renamePath)
  await dialog.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(dialog).toHaveCount(0)
  await expect(page.locator('.chat-header').getByRole('heading', { name: 'Final title!', exact: true })).toBeVisible()

  // Phones rename from the sheet, which shows the full title, and focus returns to its ⋯ button.
  await page.setViewportSize({ width: 390, height: 844 })
  ;({ action } = await openChatDetails(page))
  await expect(page.getByRole('dialog', { name: 'Chat details', exact: true })).toContainText('Final title!')
  await action('Rename conversation').click()
  await title.fill('Release notes 0.53')
  await title.press('Enter')
  await expect(dialog).toHaveCount(0)
  await expect(details).toBeFocused()
  await expect(page.getByRole('button', { name: 'Conversations', exact: true })).toContainText('Release notes 0.53')
})

test('conversation deletion is visible, confirmed and permanent on desktop and mobile', async ({ page, workspace }, testInfo) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...chat, title: 'Finalize the October release' })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('An unsent draft to remove')
  const openConversations = () => page.getByRole('button', { name: 'Conversations', exact: true }).click()

  const dialog = page.getByRole('dialog', { name: /Conversations/ })
  const confirmation = page.getByRole('dialog', { name: 'Delete conversation?', exact: true })
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  const details = page.getByRole('menu', { name: 'Chat details', exact: true })
  await expect(details.getByRole('menuitem', { name: 'Delete conversation', exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('delete-chat-details-1280.png'), animations: 'disabled' })
  await details.getByRole('menuitem', { name: 'Delete conversation', exact: true }).click()
  await expect(confirmation).toBeVisible()
  await confirmation.getByRole('button', { name: 'Keep conversation' }).click()
  await expect(page.getByRole('button', { name: 'Chat details', exact: true })).toBeFocused()
  for (const [width, colorScheme] of [[390, 'light'], [320, 'dark']] as const) {
    await page.setViewportSize({ width, height: width === 390 ? 844 : 900 })
    await page.emulateMedia({ colorScheme })
    await openConversations()
    const remove = dialog.getByRole('button', { name: 'Delete Finalize the October release', exact: true })
    await expect(remove).toBeInViewport()
    await page.screenshot({ path: testInfo.outputPath(`delete-conversations-${width}.png`), animations: 'disabled' })
    await remove.click()
    await expect(confirmation).toContainText('You cannot undo this.')
    await expect(confirmation.getByRole('button', { name: 'Keep conversation' })).toBeFocused()
    await page.screenshot({ path: testInfo.outputPath(`delete-confirmation-${width}.png`), animations: 'disabled' })
    await confirmation.getByRole('button', { name: 'Keep conversation' }).click()
    await expect(dialog.getByRole('link', { name: /Finalize the October release/ })).toBeVisible()
    await dialog.getByRole('button', { name: 'Close conversations' }).click()
  }

  await openConversations()
  await dialog.getByRole('button', { name: 'Delete Finalize the October release', exact: true }).click()
  const deletionUrl = `**/api/chats/${chat.id}`
  await page.route(deletionUrl, route => route.request().method() === 'DELETE'
    ? route.fulfill({ status: 503, json: { error: 'Deletion is temporarily unavailable. Try again.' } })
    : route.continue())
  await confirmation.getByRole('button', { name: 'Delete now', exact: true }).click()
  await expect(confirmation.getByRole('alert')).toContainText('Deletion is temporarily unavailable.')
  await expect(confirmation.getByRole('button', { name: 'Delete now', exact: true })).toBeEnabled()
  expect(workspace.service.store.get('chats', chat.id)).toBeDefined()
  await page.unroute(deletionUrl)
  await confirmation.getByRole('button', { name: 'Delete now', exact: true }).click()
  await expect(confirmation).toHaveCount(0)
  await expect(dialog).toHaveCount(0)
  await expect(page).toHaveURL(new RegExp(`${workspacePath('/chats')}$`))
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('')
  await expect.poll(() => workspace.service.store.get('chats', chat.id)).toBeUndefined()
  await openConversations()
  await expect(dialog.getByRole('link', { name: /Finalize the October release/ })).toHaveCount(0)
})

test('deletion from another client removes the open transcript', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const message = 'Private conversation removed remotely'
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: message })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.locator('.activity-message').getByText(message, { exact: true }).first()).toBeVisible({ timeout: 30000 })
  await workspace.api(`/api/chats/${chat.id}`, 'DELETE', {})
  await expect(page.locator('.activity-message').getByText(message, { exact: true })).toHaveCount(0)
  await expect.poll(() => workspace.service.store.get('chats', chat.id), { timeout: 20000 }).toBeUndefined()
  await page.reload()
  await expect(page.locator('.activity-message').getByText(message, { exact: true })).toHaveCount(0)
})

test('a failed fresh-session action keeps its confirmation available for retry', async ({ page, workspace }, testInfo) => {
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const original = workspace.service.store.runs().find(run => run.status === 'succeeded')!
  const run = workspace.service.store.addRun({
    ...original,
    id: randomUUID(),
    status: 'failed',
    trigger: 'chat',
    restoredAt: Date.now(),
  })
  workspace.service.store.put('chats', { ...chat, runId: run.id })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByRole('button', { name: 'Start a fresh agent session', exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('restored-session-offer.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Start a fresh agent session', exact: true }).click()
  let attempts = 0
  await page.route(`**/chats/${chat.id}/new-session`, async (route) => {
    attempts++
    await route.fulfill({ status: 503, json: { error: 'Installation request timed out.' } })
  })
  const confirmation = page.getByRole('dialog', { name: 'Start a fresh agent session?', exact: true })
  await confirmation.getByRole('button', { name: 'Confirm new session', exact: true }).click()
  await expect(confirmation.getByRole('status').filter({ hasText: 'Connection unavailable. Please try again.' })).toBeVisible()
  await expect(confirmation).toBeVisible()
  expect(attempts).toBe(1)
  await page.screenshot({ path: testInfo.outputPath('restored-session-retry.png'), animations: 'disabled' })
})

test('an explicit immediate result replaces the wait despite changed wording', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Check the review' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const wakeup = {
    id: randomUUID(),
    messageId: detail.run.chatExecution.messageId,
    reason: 'Original wait',
    seconds: 420,
    createdAt: Date.now(),
    wakeAt: Date.now() + 420000,
    condition: { type: 'run_finished', runId: 'dependency' },
  }
  workspace.service.store.event(detail.run.id, 'chat.wait', wakeup.reason, wakeup)
  // Real histories keep the marker on the event, independently of its display text.
  workspace.service.store.event(detail.run.id, 'chat.wait_ready', 'Result available', {
    immediate: true,
    reason: 'Review finished',
    state: 'completed',
    type: 'run_finished',
    runId: 'dependency',
    status: 'succeeded',
  })
  await workspace.restart()
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const cards = page.locator('.chat-wait')
  await expect(cards).toHaveCount(2)
  await expect(cards.first()).toContainText('Replaced by a later wait call')
  await expect(cards.last()).toContainText('The result was already available')
  await expect(cards.last()).toContainText('Review finished')
  await expect(cards.last()).toContainText('The run finished')
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await cards.last().scrollIntoViewIfNeeded()
    await expect(cards.last()).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    await page.screenshot({ path: test.info().outputPath(`immediate-wait-${width}.png`), animations: 'disabled' })
  }
})

test('a plugin condition wait reads in its plugin\'s words, with generic fallbacks', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Merge once CI passes' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const wakeup = {
    id: randomUUID(),
    messageId: detail.run.chatExecution.messageId,
    reason: 'Merge once CI passes',
    seconds: 1800,
    createdAt: Date.now() - 600000,
    wakeAt: Date.now() + 1200000,
    condition: {
      type: 'github_pr_checks',
      pluginId: 'github',
      projectId: 'project',
      prNumber: 7,
      headSha: 'a'.repeat(40),
    },
    display: { title: 'Waiting for required checks on PR #7', short: 'PR #7 checks', details: ['aaaaaaa'] },
  }
  workspace.service.store.event(detail.run.id, 'chat.wait', wakeup.reason, wakeup)
  const observation = {
    state: 'completed',
    type: 'github_pr_checks',
    pluginId: 'github',
    conclusion: 'success',
    display: { tone: 'ok', title: 'Required checks passed', detail: '4 checks' },
  }
  workspace.service.store.event(detail.run.id, 'chat.wait_ready', 'Wait condition resolved', observation)
  workspace.service.store.event(detail.run.id, 'chat.wakeup', wakeup.reason, { ...wakeup, observation })
  // A plugin result without its own words, already available when the agent asked.
  workspace.service.store.event(detail.run.id, 'chat.wait_ready', 'Wait condition already completed', {
    immediate: true,
    reason: 'Check the nightly build',
    state: 'completed',
    type: 'nightly',
    pluginId: 'ci',
    conclusion: 'action_required',
  })
  await workspace.restart()
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const cards = page.locator('.chat-wait')
  await expect(cards).toHaveCount(2)
  await expect(cards.first()).toContainText('Required checks passed')
  await expect(cards.first()).toContainText('PR #7 checks · 4 checks')
  await expect(cards.last()).toContainText('Action required')
  await expect(cards.last()).toContainText('A condition · The result was already available')
  await expect(cards.filter({ hasText: /GitHub|unreachable/ })).toHaveCount(0)
})

test('context usage shows the latest request and an explicit compaction keeps the visible history', async ({ page, workspace }, testInfo) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Measure the context' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('succeeded')
  const opened = await workspace.api(`/api/chats/${chat.id}`)
  const before = opened.run
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)

  // Codex reports the latest request (40,900 tokens), not the thread's cumulative total. The header
  // mentions the context only once the window is nearly full; the ⋯ menu always shows it.
  await expect(page.getByTestId('context-indicator')).toHaveCount(0)
  const earlierReply = page.getByRole('region', { name: 'Run conversation', exact: true })
    .getByText('Ready for the next step.', { exact: true })
  await expect(earlierReply).toBeVisible()
  let { details, action } = await openChatDetails(page)
  await expect(details.getByTestId('context-usage')).toContainText('41K / 258K · 16%')
  await expect(details.getByTestId('context-usage')).toContainText('40,900 of 258,400 tokens (16%)')
  await expect(details.getByTestId('context-usage')).toContainText('36,000 tokens read from cache')
  await page.screenshot({ path: testInfo.outputPath('context-details.png'), animations: 'disabled' })
  await action('Compact context').click()
  const confirmation = page.getByRole('dialog', { name: 'Compact the agent context?', exact: true })
  await expect(confirmation).toContainText('Every message stays visible')
  await confirmation.getByRole('button', { name: 'Compact context', exact: true }).click()
  await expect(confirmation).toHaveCount(0)

  const divider = page.getByTestId('context-compaction')
  await expect(divider).toHaveCount(1)
  await expect(divider).toContainText('Context compacted', { timeout: 15000 })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).contextUsage?.contextTokens).toBe(6200)
  ;({ details } = await openChatDetails(page))
  await expect(details.getByTestId('context-usage')).toContainText('≈ 6K / 258K · 2%')
  await closeChatDetails(page)
  // The earlier reply and request stay in the transcript; no reply or new finish is invented.
  await expect(earlierReply).toBeVisible()
  await expect(page.locator('.user-bubble').filter({ hasText: 'Measure the context' })).toBeVisible()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).compaction?.available).toBe(true)
  const after = await workspace.api(`/api/chats/${chat.id}`)
  expect(after.run.status).toBe('succeeded')
  expect(after.run.finishedAt).toBe(before.finishedAt)
  expect(after.run.summary).toBe(before.summary)
  // The compaction is not a chat message.
  expect(after.messages).toEqual(opened.messages)
  expect(after.contextUsage).toEqual({
    contextTokens: 6200,
    contextWindow: 258400,
    cachedTokens: null,
    estimated: true,
  })

  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      await divider.scrollIntoViewIfNeeded()
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
      await page.screenshot({ path: testInfo.outputPath(`context-compaction-${colorScheme}-${width}.png`), animations: 'disabled' })
    }
  }

  // On phones the details sheet carries the same measurement and action.
  ;({ details, action } = await openChatDetails(page))
  await expect(details.getByTestId('context-usage')).toContainText('Estimated by the coding agent')
  await expect(action('Compact context')).toBeEnabled()
  await page.screenshot({ path: testInfo.outputPath('context-details-phone.png'), animations: 'disabled' })
  await closeChatDetails(page)

  // A nearly full window reaches the desktop header, and compaction is one click away.
  workspace.service.store.updateRun(after.run.id, {
    contextUsage: {
      contextTokens: 227400,
      contextWindow: 258400,
      cachedTokens: null,
      estimated: false,
    },
  } as any)
  await page.setViewportSize({ width: 1280, height: 844 })
  await page.reload()
  const indicator = page.getByTestId('context-indicator')
  await expect(indicator).toHaveText('Context 88%')
  await expect(indicator).toHaveAttribute('title', /227,400 of 258,400 tokens \(88%\)/)
  // An estimate says so in the pill too.
  workspace.service.store.updateRun(after.run.id, {
    contextUsage: {
      contextTokens: 227400,
      contextWindow: 258400,
      cachedTokens: null,
      estimated: true,
    },
  } as any)
  await page.reload()
  await expect(indicator).toHaveText('Context ≈ 88%')
  await page.screenshot({ path: testInfo.outputPath('context-header-full.png'), animations: 'disabled' })
  await indicator.click()
  const confirmCompaction = page.getByRole('dialog', { name: 'Compact the agent context?', exact: true })
  await expect(confirmCompaction).toBeVisible()
  // Cancelling returns to the pill; once compaction frees the window, the pill leaves and focus goes to ⋯.
  await page.keyboard.press('Escape')
  await expect(confirmCompaction).toHaveCount(0)
  await expect(indicator).toBeFocused()
  await expect(indicator).toHaveAccessibleDescription('Opens compaction')
  await page.keyboard.press('Enter')
  await confirmCompaction.getByRole('button', { name: 'Compact context', exact: true }).click()
  await expect(confirmCompaction).toHaveCount(0)
  await expect(indicator).toHaveCount(0, { timeout: 15000 })
  await expect(page.getByRole('button', { name: 'Chat details', exact: true })).toBeFocused()

  // A focused action that becomes unavailable while the menu is open hands focus to the next one.
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).compaction?.available, { timeout: 15000 }).toBe(true)
  ;({ action } = await openChatDetails(page))
  await expect(action('Share conversation')).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(action('Rename conversation')).toBeFocused()
  await page.keyboard.press('ArrowDown')
  await expect(action('Compact context')).toBeFocused()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:chat-hang' })
  await expect(action('Compact context')).toBeDisabled({ timeout: 15000 })
  await expect(action('Open activity fullscreen')).toBeFocused()
  await page.keyboard.press('Escape')
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
})

test('compaction is unavailable while a turn runs and explains why', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:chat-hang' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('running')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const { details, action } = await openChatDetails(page)
  await expect(action('Compact context')).toBeDisabled()
  await expect(action('Compact context')).toHaveAccessibleDescription('Wait for the current turn to finish before compacting.')
  await expect(details).toContainText('Wait for the current turn to finish before compacting.')
  // The disabled action cannot take focus: the menu announces why compaction waits.
  await expect(page.getByRole('menu', { name: 'Chat details', exact: true })).toHaveAccessibleDescription(/Wait for the current turn to finish before compacting\.$/)
  await expect(workspace.api(`/api/chats/${chat.id}/compact`, 'POST', {})).rejects.toThrow(/Wait for the current turn to finish/)
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
})
