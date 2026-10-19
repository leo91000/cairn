import type { Page } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

async function expectSavedStream(page: Page, stream: string, savedAfter = 0) {
  await expect.poll(() => page.evaluate(({ stream, savedAfter }) => new Promise<boolean>((resolve) => {
    const open = indexedDB.open('cairn-history-v2')
    open.onsuccess = () => {
      const db = open.result
      const tx = db.transaction('entries')
      const request = tx.objectStore('entries').getAll()
      request.onsuccess = () => resolve(request.result.some(entry => entry.key.endsWith(stream) && entry.savedAt >= savedAfter))
      tx.oncomplete = () => db.close()
    }
  }), { stream, savedAfter })).toBe(true)
}

for (const change of ['start', 'stop'] as const) {
  test(`returning to a saved conversation keeps a ${change === 'start' ? 'running' : 'stopped'} conversation in the current Fil while reconnecting`, async ({ page, workspace }) => {
    test.setTimeout(90000)
    await useRelayForHttpMocks(page)
    await page.emulateMedia({ reducedMotion: 'reduce' })
    const first = await workspace.api('/api/chats', 'POST', {})
    const second = await workspace.api('/api/chats', 'POST', {})
    try {
      for (const [chat, text] of [[first, 'Saved navigation conversation'], [second, 'Current navigation conversation']] as const) {
        await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text })
        await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
      }

      if (change === 'stop') {
        await workspace.api(`/api/chats/${second.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep working. fixture:chat-hang' })
        await expect.poll(() => workspace.service.chats.detail(second.id).run?.status).toBe('running')
      }

      const firstDetail = await workspace.api(`/api/chats/${first.id}`)
      const secondDetail = await workspace.api(`/api/chats/${second.id}`)
      await page.setViewportSize({ width: 1440, height: 900 })
      await page.goto(workspacePath(`/chats/${firstDetail.slug}`))
      await authenticateWorkspace(page)
      await expect(page.locator('.activity-message').getByText('Saved navigation conversation', { exact: true }).first()).toBeVisible()
      await expectSavedStream(page, `/chats/${firstDetail.slug}/stream`)

      const fil = page.getByRole('complementary', { name: 'Fil' })
      const secondRow = fil.locator(`[data-fil-key="chat:${second.id}"]`)
      await secondRow.click()
      await expect(page).toHaveURL(workspacePath(`/chats/${secondDetail.slug}`))
      await expect(page.locator('.activity-message').getByText('Current navigation conversation', { exact: true }).first()).toBeVisible()
      if (change === 'start')
        await workspace.api(`/api/chats/${second.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep working. fixture:chat-hang' })
      else
        await workspace.api(`/api/chats/${second.id}/stop`, 'POST')
      const currentSection = fil.getByRole('region', { name: change === 'start' ? 'Live' : 'Recent', exact: true })
      await expect(currentSection.locator(`[data-fil-key="chat:${second.id}"]`)).toBeVisible()
      await expect(secondRow).toHaveCount(1)
      const order = await fil.locator('[data-fil-key]').evaluateAll(rows => rows.map(row => row.getAttribute('data-fil-key')))

      // A slow or disconnected target still restores its messages from disk, but must
      // not restore an old installation-wide list over the state just seen on screen.
      await page.route(`**/api/chats/${firstDetail.slug}/stream?**`, route => route.abort())
      await page.route(`**/api/chats/${firstDetail.slug}`, route => route.abort())
      await fil.locator(`[data-fil-key="chat:${first.id}"]`).click()
      await expect(page.locator('.activity-message').getByText('Saved navigation conversation', { exact: true }).first()).toBeVisible()
      await expect(currentSection.locator(`[data-fil-key="chat:${second.id}"]`)).toBeVisible()
      await expect(fil.getByRole('region', { name: change === 'start' ? 'Recent' : 'Live', exact: true }).locator(`[data-fil-key="chat:${second.id}"]`)).toHaveCount(0)
      await expect(secondRow).toHaveCount(1)
      expect(await fil.locator('[data-fil-key]').evaluateAll(rows => rows.map(row => row.getAttribute('data-fil-key')))).toEqual(order)
      await page.screenshot({ path: test.info().outputPath(`navigation-${change}-after.png`), animations: 'disabled' })

      await page.unrouteAll({ behavior: 'wait' })
      await useRelayForHttpMocks(page)
      await page.reload()
      await expect(currentSection.locator(`[data-fil-key="chat:${second.id}"]`)).toBeVisible()
    }
    finally {
      for (const chat of [first, second])
        await workspace.api(`/api/chats/${chat.id}`, 'DELETE', { confirm: true })
    }
  })
}

for (const view of ['conversation', 'fil'] as const) {
  test(`reloading the saved ${view} offline restores the Fil`, async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    const chat = await workspace.api('/api/chats', 'POST', {})
    try {
      await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Saved offline navigation conversation' })
      await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
      const detail = await workspace.api(`/api/chats/${chat.id}`)
      const path = view === 'conversation' ? `/chats/${detail.slug}` : '/'
      const stream = view === 'conversation' ? `/chats/${detail.slug}/stream` : '/chats/stream'
      await page.setViewportSize({ width: 1440, height: 900 })
      await page.goto(workspacePath(path))
      await authenticateWorkspace(page)
      const row = page.locator(`.fil [data-fil-key="chat:${chat.id}"]`)
      await expect(row).toBeVisible()
      await expectSavedStream(page, stream)

      await page.route('**/api/chats**', route => route.abort())
      await page.reload()
      await expect(row).toBeVisible()
      // Persist the restored reading position before another offline load.
      const savedAfter = await page.evaluate(() => {
        const now = Date.now()
        window.dispatchEvent(new PageTransitionEvent('pagehide'))
        return now
      })
      await expectSavedStream(page, stream, savedAfter)
      await page.reload()
      await expect(row).toBeVisible()
      if (view === 'conversation')
        await expect(page.locator('.activity-message').getByText('Saved offline navigation conversation', { exact: true }).first()).toBeVisible()
    }
    finally {
      await workspace.api(`/api/chats/${chat.id}`, 'DELETE', { confirm: true })
    }
  })
}

// Samples every animation frame while switching conversations: which conversation the
// URL selects, which one the reader shows, and whether the shell stays mounted.
async function recordFrames(page: Page) {
  await page.evaluate(() => {
    const w = window as any
    w.__frames = []
    document.querySelector('aside[aria-label="Fil"]')!.setAttribute('data-probe', '1')
    document.querySelector('textarea')!.setAttribute('data-probe', '1')
    const sample = () => {
      const heading = document.querySelector('.chat-page h1:not(.sr-only)')
      w.__frames.push({
        path: location.pathname,
        loading: !!Array.from(document.querySelectorAll('[role="status"]')).some(e => e.textContent?.includes('Loading conversation')),
        filRemounted: !document.querySelector('aside[aria-label="Fil"][data-probe]'),
        composerRemounted: !document.querySelector('textarea[data-probe]'),
        filItems: document.querySelectorAll('aside[aria-label="Fil"] [data-fil-key]').length,
        title: heading?.textContent?.trim() ?? '',
        messages: Array.from(document.querySelectorAll('.activity-message'), message => message.textContent ?? ''),
      })
      if (w.__frames.length < 120)
        requestAnimationFrame(sample)
    }

    requestAnimationFrame(sample)
  })
}

test('switching conversations never shows the previous one and reopens saved ones at once', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const first = await workspace.api('/api/chats', 'POST', {})
  const second = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${first.id}/messages`, 'POST', { id: randomUUID(), text: 'First conversation message' })
  await workspace.api(`/api/chats/${second.id}/messages`, 'POST', { id: randomUUID(), text: 'Second conversation message' })
  await expect.poll(() => workspace.service.chats.detail(first.id).run?.status).toBe('succeeded')
  await expect.poll(() => workspace.service.chats.detail(second.id).run?.status).toBe('succeeded')
  workspace.service.store.put('chats', { ...workspace.service.chats.detail(first.id), title: 'Alpha conversation' })
  workspace.service.store.put('chats', { ...workspace.service.chats.detail(second.id), title: 'Beta conversation' })
  await page.setViewportSize({ width: 1440, height: 900 })
  // Opened by identifier: the copy is saved where the Fil's slug link finds it.
  await page.goto(workspacePath(`/chats/${first.id}`))
  await authenticateWorkspace(page)
  await expect(page.locator('.activity-message').getByText('First conversation message', { exact: true }).first()).toBeVisible({ timeout: 30000 })
  const fil = page.getByRole('complementary', { name: 'Fil' })
  await expect(fil.locator('[data-fil-key]')).toHaveCount(2)
  const switches = [
    ['Beta conversation', 'Second conversation message', 'First conversation message', 'first visit'],
    ['Alpha conversation', 'First conversation message', 'Second conversation message', 'saved'],
    ['Beta conversation', 'Second conversation message', 'First conversation message', 'saved'],
  ] as const
  for (const [target, text, previous, visit] of switches) {
    await recordFrames(page)
    await fil.getByText(target, { exact: true }).first().click()
    await expect(page.locator('.activity-message').getByText(text, { exact: true }).first()).toBeVisible()
    await page.waitForFunction(() => (window as any).__frames.length >= 120)
    const frames: any[] = await page.evaluate(() => (window as any).__frames)
    const selected = frames.filter(f => f.path === new URL(page.url()).pathname)
    expect(selected.length).toBeGreaterThan(0)
    // Once the target is selected, the previous conversation never shows again, and the
    // header names the target from the Fil while it opens.
    expect(selected.filter(f => f.messages.some((message: string) => message.includes(previous)))).toEqual([])
    expect(selected.every(f => f.title === target)).toBe(true)
    // A conversation read earlier in the page shows in the first frame after the click.
    if (visit === 'saved') {
      expect(selected.filter(f => f.loading || !f.messages.length)).toEqual([])
    }

    expect(frames.some(f => f.filRemounted || f.composerRemounted)).toBe(false)
    expect(Math.min(...frames.map(f => f.filItems))).toBeGreaterThan(0)
  }
})

test('a slow conversation opens on a loader, then returns to the previous reading position at once', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const first = await workspace.api('/api/chats', 'POST', {})
  const second = await workspace.api('/api/chats', 'POST', {})
  for (const [chat, text] of [[first, 'Keep this conversation saved'], [second, 'Slow target conversation']] as const) {
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text })
    await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  }

  const firstRun = workspace.service.chats.detail(first.id).run!
  for (let index = 0; index < 30; index++) {
    const text = `Reading paragraph ${index}. Keep the reading position while the next conversation loads.`
    workspace.service.store.event(firstRun.id, 'item.completed', text, { item: { id: `reading-${index}`, type: 'agent_message', text } })
  }

  await page.goto(workspacePath(`/chats/${first.slug}`))
  await authenticateWorkspace(page)
  const previous = page.locator('.activity-message').getByText('Keep this conversation saved', { exact: true }).first()
  await expect(previous).toBeAttached()
  const scroller = page.getByRole('region', { name: 'Activity output' })
  await scroller.evaluate(element => element.scrollTop = 300)
  await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBe(300)
  let release!: () => void
  const gate = new Promise<void>(resolve => release = resolve)
  await page.route(`**/api/chats/${second.slug}**`, async (route) => {
    await gate
    await route.continue()
  })
  try {
    await page.locator(`[data-fil-key="chat:${second.id}"]`).click()
    await expect(page).toHaveURL(workspacePath(`/chats/${second.slug}`))
    // The previous conversation leaves at once; a centred loader takes its place.
    await expect(previous).toHaveCount(0)
    await expect(page.locator('.activity-message')).toHaveCount(0)
    const loader = page.getByRole('status').filter({ hasText: 'Loading conversation' })
    await expect(loader).toBeVisible()
    await expect(loader).toHaveCSS('opacity', '1')
    await expect(page.locator('.chat-page h1:not(.sr-only)')).toHaveText(workspace.service.chats.detail(second.id).title)
    await page.screenshot({ path: test.info().outputPath('slow-navigation-pending.png'), animations: 'disabled' })
    release()
    await expect(page.locator('.activity-message').getByText('Slow target conversation', { exact: true }).first()).toBeVisible()
    await expect(loader).toHaveCount(0)
    await page.screenshot({ path: test.info().outputPath('slow-navigation-ready.png'), animations: 'disabled' })
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'ignoreErrors' })
  }

  // Going back shows the saved conversation where the reader left it, without a loader.
  await page.locator(`[data-fil-key="chat:${first.id}"]`).click()
  await expect(previous).toBeAttached()
  await expect(scroller).toHaveJSProperty('scrollTop', 300)
  await expect(page.getByRole('status').filter({ hasText: 'Loading conversation' })).toHaveCount(0)
})

test('an outdated saved conversation waits for its update instead of showing older output', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const first = await workspace.api('/api/chats', 'POST', {})
  const second = await workspace.api('/api/chats', 'POST', {})
  for (const [chat, text] of [[first, 'Outdated conversation question'], [second, 'Another conversation']] as const) {
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text })
    await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  }

  await page.goto(workspacePath(`/chats/${first.slug}`))
  await authenticateWorkspace(page)
  await expect(page.locator('.activity-message').getByText('Outdated conversation question', { exact: true }).first()).toBeVisible()
  await page.locator(`[data-fil-key="chat:${second.id}"]`).click()
  await expect(page.locator('.activity-message').getByText('Another conversation', { exact: true }).first()).toBeVisible()

  // The first conversation finishes another turn while the reader is elsewhere.
  await workspace.api(`/api/chats/${first.id}/messages`, 'POST', { id: randomUUID(), text: 'A later question' })
  await expect.poll(() => workspace.service.chats.detail(first.id).messages.some(message => message.text === 'A later question' && message.status === 'delivered')).toBe(true)
  await expect.poll(() => workspace.service.chats.detail(first.id).run?.status).toBe('succeeded')
  // The Fil's list stream reports the finished turn the reader has not opened yet.
  const row = page.locator(`[data-fil-key="chat:${first.id}"][data-unread]`)
  await expect(row).toBeAttached()

  let release!: () => void
  const gate = new Promise<void>(resolve => release = resolve)
  const streams: string[] = []
  await page.route(`**/api/chats/${first.slug}/stream**`, async (route) => {
    streams.push(route.request().url())
    await gate
    await route.continue()
  })
  try {
    await row.click()
    // The Fil already shows the later turn: the saved copy only resumes the stream.
    await expect(page.getByRole('status').filter({ hasText: 'Loading conversation' })).toBeVisible()
    await expect(page.locator('.activity-message')).toHaveCount(0)
    await expect.poll(() => streams.length).toBeGreaterThan(0)
    expect(Number(new URL(streams[0]!).searchParams.get('after'))).toBeGreaterThan(0)
    release()
    await expect(page.locator('.activity-message').getByText('A later question', { exact: true }).first()).toBeVisible()
    await expect(page.locator('.activity-message').getByText('Outdated conversation question', { exact: true }).first()).toBeAttached()
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'ignoreErrors' })
  }
})

test('opening a mission from a conversation keeps the Fil mounted and visible', async ({ page, workspace }) => {
  await page.setViewportSize({ width: 1440, height: 900 })
  const run = workspace.service.store.runs().find(run => run.trigger !== 'chat')!
  workspace.service.store.updateRun(run.id, { status: 'failed', error: 'Navigation fixture' })
  const mission = workspace.service.store.get('tasks', run.taskId)!
  workspace.service.store.put('tasks', { ...mission, name: 'Navigation mission' })
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Navigation shell proof' })
  await page.goto(workspacePath(`/chats/${chat.slug}`))
  await authenticateWorkspace(page)
  await expect(page.locator('.activity-message').getByText('Navigation shell proof', { exact: true }).first()).toBeVisible()
  const fil = page.locator('.fil')
  await fil.evaluate(element => element.setAttribute('data-navigation-probe', 'mounted'))
  await fil.getByRole('textbox', { name: 'Filter conversations' }).fill('Navigation')
  const width = (await fil.boundingBox())!.width
  await page.locator(`[data-fil-key="task:${run.taskId}"]`).getByRole('button', { name: /^Open/ }).click()
  await expect(page).toHaveURL(workspacePath(`/fil/runs/${run.id}`))
  await expect(fil).toHaveAttribute('data-navigation-probe', 'mounted')
  await expect(fil.getByRole('textbox', { name: 'Filter conversations' })).toHaveValue('Navigation')
  expect((await fil.boundingBox())!.width).toBe(width)
  expect(await fil.evaluate((element) => {
    let opacity = 1
    for (let parent: Element | null = element; parent; parent = parent.parentElement)
      opacity *= Number(getComputedStyle(parent).opacity)
    return opacity
  })).toBe(1)
  await page.screenshot({ path: test.info().outputPath('mission-navigation-ready.png'), animations: 'disabled' })
  // Once read, the conversation moves out of « For you »: click it where it settles.
  const conversation = page.locator(`[data-fil-key="chat:${chat.id}"]`)
  await expect(conversation).toHaveCount(1)
  await conversation.click()
  await expect(page.locator('.activity-message').getByText('Navigation shell proof', { exact: true }).first()).toBeVisible()
  await expect(fil).toHaveAttribute('data-navigation-probe', 'mounted')
  await expect(fil.getByRole('textbox', { name: 'Filter conversations' })).toHaveValue('Navigation')
})
