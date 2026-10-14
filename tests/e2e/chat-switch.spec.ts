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
    const open = indexedDB.open('cairn-history-v1', 1)
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

// Samples every animation frame while switching conversations. A seamless switch never
// shows the loading placeholder, never empties the Fil and keeps the shell mounted.
async function recordFrames(page: Page) {
  await page.evaluate(() => {
    const w = window as any
    w.__frames = []
    document.querySelector('aside[aria-label="Fil"]')!.setAttribute('data-probe', '1')
    document.querySelector('textarea')!.setAttribute('data-probe', '1')
    const sample = () => {
      const heading = document.querySelector('.chat-page h1:not(.sr-only)')
      w.__frames.push({
        loading: !!Array.from(document.querySelectorAll('[role="status"]')).some(e => e.textContent?.includes('Loading conversation')),
        filRemounted: !document.querySelector('aside[aria-label="Fil"][data-probe]'),
        composerRemounted: !document.querySelector('textarea[data-probe]'),
        filItems: document.querySelectorAll('aside[aria-label="Fil"] [data-fil-key]').length,
        title: heading?.textContent?.trim() ?? '',
        messages: document.querySelectorAll('.activity-message').length,
      })
      if (w.__frames.length < 120)
        requestAnimationFrame(sample)
    }

    requestAnimationFrame(sample)
  })
}

test('switching conversations is seamless', async ({ page, workspace }) => {
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
  await page.goto(workspacePath(`/chats/${first.id}`))
  await authenticateWorkspace(page)
  await expect(page.locator('.activity-message').getByText('First conversation message', { exact: true }).first()).toBeVisible({ timeout: 30000 })
  const fil = page.getByRole('complementary', { name: 'Fil' })
  // The first message batch can precede the initial conversation list.
  await expect(fil.locator('[data-fil-key]')).toHaveCount(2)
  const summaries: Record<string, unknown>[] = []
  for (const [target, text] of [['Beta conversation', 'Second conversation message'], ['Alpha conversation', 'First conversation message'], ['Beta conversation', 'Second conversation message']] as const) {
    await recordFrames(page)
    await fil.getByText(target, { exact: true }).first().click()
    await expect(page.locator('.activity-message').getByText(text, { exact: true }).first()).toBeVisible()
    await page.waitForFunction(() => (window as any).__frames.length >= 120)
    const frames: any[] = await page.evaluate(() => (window as any).__frames)
    const summary = {
      target,
      loadingFrames: frames.filter(f => f.loading).length,
      emptyTitleFrames: frames.filter(f => !f.title).length,
      noMessageFrames: frames.filter(f => !f.messages).length,
      minFilItems: Math.min(...frames.map(f => f.filItems)),
      filRemounted: frames.some(f => f.filRemounted),
      composerRemounted: frames.some(f => f.composerRemounted),
    }
    summaries.push(summary)
  }

  for (const summary of summaries) {
    expect(summary).toMatchObject({
      loadingFrames: 0,
      emptyTitleFrames: 0,
      noMessageFrames: 0,
      filRemounted: false,
      composerRemounted: false,
    })
    expect(summary.minFilItems).toBeGreaterThan(0)
  }
})
