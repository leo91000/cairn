import type { Page } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  test,
  workspacePath,
} from './fixtures'

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
