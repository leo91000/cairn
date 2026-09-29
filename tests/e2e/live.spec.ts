import type { BrowserContext, Page } from '@playwright/test'
import { randomUUID } from 'node:crypto'
import { expect, expectChatReady, expectSingleScroll, initializeRepository, test } from './fixtures'

for (const kind of ['chat', 'task'] as const) {
  test(`${kind} automatically fills folded history without a click or scroll`, async ({ page, workspace }) => {
    await page.setViewportSize(kind === 'chat' ? { width: 390, height: 844 } : { width: 1440, height: 1100 })
    let run = workspace.service.store.runs().find(run => run.status === 'succeeded' && run.trigger !== 'chat')!
    let path = `/runs/${run.id}`
    if (kind === 'chat') {
      const chat = await workspace.api('/api/chats', 'POST', {})
      await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Short history fixture' })
      await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
      const detail = await workspace.api(`/api/chats/${chat.id}`)
      run = workspace.service.store.run(detail.run.id)!
      path = `/chats/${chat.id}`
    }
    for (let i = 0; i < 100; i++) {
      const text = `Buffered message ${i}: earlier conversation context.`
      workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `buffer-${i}`, type: 'agent_message', text } })
    }
    // Several API pages collapse into one tool group, leaving no scrollbar.
    for (let i = 0; i < 250; i++) {
      workspace.service.store.event(run.id, 'item.completed', 'Checked a file', { item: { id: `fold-${i}`, type: 'command_execution', command: 'cat README.md', exit_code: 0 } })
    }
    workspace.service.store.event(run.id, 'item.completed', 'Latest short answer', { item: { id: 'latest-short', type: 'agent_message', text: 'Latest short answer' } })
    const requests: string[] = []
    page.on('request', (request) => {
      if (request.url().includes('/history?'))
        requests.push(request.url())
    })
    await page.goto(`${workspace.url}${path}`)
    await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
    await page.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(page.locator('.activity-message').filter({ hasText: 'Latest short answer' })).toBeVisible()
    await expect(page.locator('.activity-message').filter({ hasText: 'Buffered message 99:' })).toBeAttached()
    expect(requests.length).toBeGreaterThanOrEqual(2)
    expect(new Set(requests).size).toBe(requests.length)
    // Loading older pages must keep following the latest output.
    const scroller = page.getByRole('region', { name: 'Activity output' })
    await expect.poll(() => scroller.evaluate(el => el.scrollHeight - el.clientHeight - el.scrollTop)).toBeLessThan(2)
    if (kind === 'chat')
      await expect(page.getByRole('button', { name: 'Follow output', exact: true, includeHidden: true })).toHaveAttribute('aria-pressed', 'true')
    else
      await expect(page.getByLabel('Follow output')).toBeChecked()
    await page.screenshot({ path: test.info().outputPath(`${kind}-automatic-history.png`), animations: 'disabled' })
  })
}

test('automatic history pauses on errors and resumes only after retry', async ({ page, workspace }) => {
  const run = workspace.service.store.runs().find(run => run.status === 'succeeded' && run.trigger !== 'chat')!
  for (let i = 0; i < 220; i++)
    workspace.service.store.event(run.id, 'item.completed', 'Checked a file', { item: { id: `retry-${i}`, type: 'command_execution', command: 'cat README.md', exit_code: 0 } })
  let requests = 0
  let fail = true
  await page.route('**/history?**', async (route) => {
    requests++
    if (fail)
      await route.fulfill({ status: 503, json: { error: 'History temporarily unavailable' } })
    else
      await route.continue()
  })
  await page.goto(`${workspace.url}/runs/${run.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  const retry = page.getByRole('button', { name: 'Retry', exact: true })
  await expect(retry).toBeVisible()
  expect(requests).toBe(1)
  // Both resize and scroll normally trigger prefetch; neither should retry errors.
  await page.setViewportSize({ width: 1440, height: 1100 })
  await page.getByRole('region', { name: 'Activity output' }).evaluate(async (el) => {
    el.dispatchEvent(new Event('scroll'))
    await new Promise(requestAnimationFrame)
    await new Promise(requestAnimationFrame)
  })
  expect(requests).toBe(1)
  fail = false
  await retry.click()
  await expect(retry).not.toBeVisible()
  await expect.poll(() => requests).toBeGreaterThanOrEqual(3)
  await expect(page.getByLabel('Follow output')).toBeChecked()
})

test('signing out closes live subscriptions before the session is revoked', async ({ page, workspace }) => {
  await page.goto(workspace.url)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.getByRole('link', { name: 'New conversation', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'What are we building?' })).toBeVisible()
  const unauthorized: string[] = []
  page.on('response', (response) => {
    if (response.status() === 401)
      unauthorized.push(response.url())
  })
  await page.route('**/api/logout', async (route) => {
    const response = await route.fetch()
    // Leave the UI mounted after revocation: an open stream would immediately
    // close and trigger its access check before the logout response arrives.
    await new Promise(resolve => setTimeout(resolve, 800))
    await route.fulfill({ response })
  })
  await page.getByRole('button', { name: 'Sign out', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Sign in', exact: true })).toBeVisible()
  expect(unauthorized).toEqual([])
})

test('two independent clients follow deltas, recover offline, refresh mid-answer and survive a server restart', async ({ page, browser, workspace }) => {
  test.setTimeout(90000)
  initializeRepository(workspace.projectPath)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const contexts: BrowserContext[] = []
  const signIn = async (target: Page) => {
    await target.goto(workspace.url)
    await target.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
    await target.getByRole('button', { name: 'Sign in', exact: true }).click()
    await expect(target.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
    await target.goto(`${workspace.url}/chats/${chat.id}`)
  }
  const message = (target: Page) => target.locator('.activity-message').filter({ hasText: 'Streaming proof:' })
  const requests: string[] = []
  page.on('request', (request) => {
    if (request.url().includes('/api/'))
      requests.push(request.url())
  })
  try {
    const second = await browser.newContext({ baseURL: workspace.url })
    contexts.push(second)
    const other = await second.newPage()
    await Promise.all([signIn(page), signIn(other)])
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:stream' })
    await expect(message(page)).toContainText('005')
    await expect(message(other)).toContainText('005')
    await second.setOffline(true)
    await expect(message(page)).toContainText('020')
    await expect(other.getByRole('status').filter({ hasText: /Offline|Reconnecting/ })).toBeVisible()
    await second.setOffline(false)
    await expect(message(other)).toContainText('025')
    await page.reload()
    await expect(message(page)).toContainText('035')
    await expect(message(page)).not.toContainText('100')
    await page.setViewportSize({ width: 390, height: 844 })
    await page.emulateMedia({ colorScheme: 'dark' })
    await expectSingleScroll(page)
    await page.screenshot({ path: test.info().outputPath('stream-mobile-dark.png'), animations: 'disabled' })
    await expect(message(page)).toContainText('100')
    await expect(message(other)).toContainText('100')
    await expect(message(page)).toHaveCount(1)
    await expect(message(other)).toHaveCount(1)
    expect(await message(page).textContent()).toBe(await message(other).textContent())
    await expectChatReady(page)
    // No event/metadata polling is required while watching the conversation.
    expect(requests.some(url => /\/events\?|\/artifacts$/.test(url))).toBe(false)
    await workspace.restart()
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'After server restart' })
    await expect(page.locator('.activity-message').filter({ hasText: 'After server restart' })).toHaveCount(2)
    await expect(other.locator('.activity-message').filter({ hasText: 'After server restart' })).toHaveCount(2)
    await expect(message(page)).toHaveCount(1)
    await expect(message(other)).toHaveCount(1)
    const third = await browser.newContext({ baseURL: workspace.url })
    contexts.push(third)
    const late = await third.newPage()
    await signIn(late)
    await expect(message(late)).toHaveCount(1)
    await expect(message(late)).toContainText('100')
    await expect(late.locator('.activity-message').filter({ hasText: 'After server restart' })).toHaveCount(2)
    // Route changes dispose old streams and cannot mix conversations.
    const empty = await workspace.api('/api/chats', 'POST', {})
    await page.goto(`${workspace.url}/chats/${empty.id}`)
    await expect(message(page)).toHaveCount(0)
    await page.goto(`${workspace.url}/chats/${chat.id}`)
    await expect(message(page)).toHaveCount(1)
  }
  finally {
    for (const context of contexts) await context.close()
  }
})

test('cached history survives reload before a delayed stream, then clear on logout', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Cache persistence proof' })
  await page.goto(`${workspace.url}/chats/${chat.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Cache persistence proof' }).first()).toBeVisible()
  const count = () => page.evaluate(() => new Promise<number>((resolve, reject) => {
    const request = indexedDB.open('leo-history-v1', 1)
    request.onsuccess = () => {
      const db = request.result
      const tx = db.transaction('entries', 'readonly')
      const read = tx.objectStore('entries').count()
      read.onsuccess = () => resolve(read.result)
      tx.oncomplete = () => db.close()
    }
    request.onerror = () => reject(request.error)
  }))
  await expect.poll(count).toBeGreaterThan(0)
  const requests: string[] = []
  let release!: () => void
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(`**/api/chats/${chat.id}/stream?**`, async (route) => {
    requests.push(route.request().url())
    await gate
    try {
      await route.continue()
    }
    catch (error) {
      // pageshow may replace an EventSource while its request is deliberately held.
      if (!(error instanceof Error) || !error.message.includes('Route is already handled'))
        throw error
    }
  })
  try {
    await page.reload()
    await expect(page.locator('.activity-message').filter({ hasText: 'Cache persistence proof' }).first()).toBeVisible()
    await expect.poll(() => requests.length).toBeGreaterThan(0)
    expect(new URL(requests[0]).searchParams.get('history')).toMatch(/^v1:/)
    expect(Number(new URL(requests[0]).searchParams.get('after'))).toBeGreaterThan(0)
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'wait' })
  }
  await page.getByRole('button', { name: 'Sign out', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Sign in', exact: true })).toBeVisible()
  await expect.poll(count).toBe(0)
})

test('a cached run restores the reading offset while its stream is still connecting', async ({ page, workspace }) => {
  const run = workspace.service.store.runs()[0]
  for (let i = 0; i < 40; i++) {
    const text = `Saved reading position ${i}. A longer paragraph to exercise the scrolling activity view across reloads.`
    workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `cache-${i}`, type: 'agent_message', text } })
  }
  await page.goto(`${workspace.url}/runs/${run.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Saved reading position 39.' })).toBeVisible()
  await page.getByLabel('Follow output').uncheck()
  const scroller = page.getByRole('region', { name: 'Activity output' })
  await scroller.evaluate(element => element.scrollTop = 300)
  await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBe(300)
  await expect.poll(() => page.evaluate(() => new Promise<number>((resolve) => {
    const request = indexedDB.open('leo-history-v1', 1)
    request.onsuccess = () => {
      const db = request.result
      const tx = db.transaction('entries', 'readonly')
      const read = tx.objectStore('entries').getAll()
      read.onsuccess = () => resolve(read.result.map(entry => JSON.parse(entry.text).position?.top).find(top => top === 300) ?? -1)
      tx.oncomplete = () => db.close()
    }
  }))).toBe(300)
  // A previous visit may have cached "running" just before the run completed.
  await page.goto(`${workspace.url}/agents`)
  await page.evaluate(() => new Promise<void>((resolve, reject) => {
    const request = indexedDB.open('leo-history-v1', 1)
    request.onsuccess = () => {
      const db = request.result
      const tx = db.transaction('entries', 'readwrite')
      const store = tx.objectStore('entries')
      const read = store.getAll()
      read.onsuccess = () => {
        for (const entry of read.result) {
          const value = JSON.parse(entry.text)
          if (value.state.run)
            value.state.run.status = 'running'
          store.put({ ...entry, text: JSON.stringify(value) })
        }
      }
      tx.oncomplete = () => {
        db.close()
        resolve()
      }
      tx.onerror = () => reject(tx.error)
    }
  }))
  let release!: () => void
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  await page.route(`**/api/runs/${run.id}/stream?**`, async (route) => {
    await gate
    try {
      await route.continue()
    }
    catch (error) {
      // pageshow may replace an EventSource while its request is deliberately held.
      if (!(error instanceof Error) || !error.message.includes('Route is already handled'))
        throw error
    }
  })
  try {
    await page.goto(`${workspace.url}/runs/${run.id}`)
    // A run opens on its conversation, restoring the cached reading offset before the stream connects.
    await expect(page.getByRole('button', { name: /^Conversation/ })).toHaveAttribute('aria-pressed', 'true')
    await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBe(300)
    await expect(page.getByLabel('Follow output')).not.toBeChecked()
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'wait' })
  }
  await expect(page.getByRole('status').filter({ hasText: 'Updating…' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /^Conversation/ })).toHaveAttribute('aria-pressed', 'true')
})

test('recent history loads older pages without moving the reader and survives a blocked reconnect', async ({ page, workspace }) => {
  const run = workspace.service.store.runs()[0]
  for (let i = 0; i < 350; i++) {
    const text = `Paged line ${String(i).padStart(3, '0')}. A paragraph to retain a stable reading position.`
    workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `page-${i}`, type: 'agent_message', text } })
  }
  await page.goto(`${workspace.url}/runs/${run.id}`)
  await page.getByLabel('Password', { exact: true }).fill('browser-password-long-enough')
  await page.getByRole('button', { name: 'Sign in', exact: true }).click()
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 349' })).toBeVisible()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 249' })).toHaveCount(0)
  await page.getByLabel('Follow output').uncheck()
  let release!: () => void
  const gate = new Promise<void>((resolve) => {
    release = resolve
  })
  let requested = false
  await page.route('**/history?**', async (route) => {
    requested = true
    await gate
    await route.continue()
  })
  const scroller = page.getByRole('region', { name: 'Activity output' })
  // Prefetch before reaching the start, as on Android.
  await scroller.evaluate(el => el.scrollTop = 2 * el.clientHeight)
  await expect.poll(() => requested).toBe(true)
  // Continue scrolling while the older page is in flight.
  await scroller.evaluate(el => el.scrollTop -= 120)
  const anchor = page.locator('.activity-message').filter({ hasText: 'Paged line 250' })
  const top = (await anchor.boundingBox())!.y
  release()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 150' })).toBeAttached()
  await expect.poll(async () => Math.abs((await anchor.boundingBox())!.y - top)).toBeLessThan(2)
  await page.unrouteAll({ behavior: 'wait' })
  // Move back to the latest content and let its bounded disk snapshot commit.
  await page.getByLabel('Follow output').check()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 349' })).toBeVisible()
  await expect.poll(() => page.evaluate(() => new Promise<number>((resolve) => {
    const open = indexedDB.open('leo-history-v1', 1)
    open.onsuccess = () => {
      const db = open.result
      const tx = db.transaction('entries')
      const request = tx.objectStore('entries').getAll()
      request.onsuccess = () => resolve(Math.max(0, ...request.result.map(e => JSON.parse(e.text).events.length)))
      tx.oncomplete = () => db.close()
    }
  }))).toBe(200)
  await page.route(`**/api/runs/${run.id}/stream?**`, route => route.abort())
  await page.reload()
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 349' })).toBeVisible()
})
