import type { BrowserContext, Page } from '@playwright/test'
import type { RunEvent } from '../../packages/contracts/contracts'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectChatReady,
  expectFollowing,
  expectSingleScroll,
  initializeRepository,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('chat groups visible calls despite diagnostics, updates and global event ID gaps', async ({ page, workspace }) => {
  const original = workspace.service.store.runs().find(run => run.status === 'succeeded')!
  const run = workspace.service.store.addRun({
    ...original,
    id: randomUUID(),
    taskId: randomUUID(),
    trigger: 'grouping-fixture',
  })
  const chat = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...chat, runId: run.id, title: 'Visible tool groups' })
  for (let i = 0; i < 65; i++) {
    const item = { id: `group-tool-${i}`, type: 'command_execution', command: `echo grouping-${i}` }
    workspace.service.store.event(run.id, 'item.started', '', { item: { ...item, status: 'in_progress' } })
    workspace.service.store.event(run.id, 'diagnostic', 'heartbeat')
    workspace.service.store.event(original.id, 'diagnostic', 'Other conversation')
    workspace.service.store.event(run.id, 'item.updated', '', { item })
    workspace.service.store.event(run.id, 'item.completed', '', { item: { ...item, exit_code: 0 } })
  }

  // Keep this test about grouping rather than paging; paging has a separate modal/anchor test.
  const events = workspace.service.store.events(run.id, 0, 500) as RunEvent[]
  await useRelayForHttpMocks(page)
  await page.route(`**/chats/${chat.id}/stream*`, route => route.fulfill({
    contentType: 'text/event-stream',
    body: `event: batch\ndata: ${JSON.stringify({
      events,
      state: { run, chat },
      reset: true,
      more: false,
      history: `grouping:${run.id}`,
      hasOlder: false,
    })}\n\n`,
  }))
  await page.goto(workspacePath(`${workspace.url}/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const groups = page.getByTestId('agent-actions')
  await expect(groups).toHaveCount(3)
  expect(await groups.allTextContents()).toEqual([
    expect.stringContaining('Ran 30 commands'),
    expect.stringContaining('Ran 30 commands'),
    expect.stringContaining('Ran 5 commands'),
  ])
})

test('markdown tables wrap prose and keep wide columns reachable within the conversation', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const run = workspace.service.store.runs().find(run => run.status === 'succeeded' && run.trigger !== 'chat')!
  const description = 'Missing or running required checks keep waiting; required failure wakes the agent. Optional checks do not block. Push a new commit or close the PR and the old wait should report that change.'
  const longPath = `https://example.test/${'long-path-segment/'.repeat(20)}`
  const columns = Array.from({ length: 12 }, (_, i) => `Column ${i + 1}`)
  const text = [
    '| Feature | What to verify |',
    '| --- | --- |',
    `| Waiting for required PR checks | ${description} |`,
    `| Long link and code | [Reference](${longPath}) and \`${longPath}\` |`,
    '',
    'The paragraph after the table stays readable.',
    '',
    `| ${columns.join(' | ')} |`,
    `| ${columns.map(() => '---').join(' | ')} |`,
    `| ${columns.map((_, i) => `Value ${i + 1}`).join(' | ')} |`,
  ].join('\n')
  workspace.service.store.event(run.id, 'item.completed', text, { item: { id: 'table-layout', type: 'agent_message', text } })
  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await authenticateWorkspace(page)
  const message = page.locator('.activity-message').filter({ hasText: 'The paragraph after the table stays readable.' })
  await expect(message.getByRole('table')).toHaveCount(2)

  for (const width of [1488, 390, 320]) {
    await page.setViewportSize({ width, height: 900 })
    const proseTable = message.getByRole('table').first()
    const layout = await proseTable.evaluate((table) => {
      const markdown = table.closest('.markdown')!
      const cell = table.querySelector('tbody tr td:last-child')!
      const range = document.createRange()
      range.selectNodeContents(cell)
      return {
        tableWidth: table.getBoundingClientRect().width,
        messageWidth: markdown.clientWidth,
        messageOverflow: markdown.scrollWidth - markdown.clientWidth,
        textLines: range.getClientRects().length,
        cellFont: getComputedStyle(cell).fontSize,
        messageFont: getComputedStyle(markdown).fontSize,
      }
    })
    expect(layout.tableWidth).toBeLessThanOrEqual(layout.messageWidth + 1)
    expect(layout.messageOverflow).toBeLessThanOrEqual(1)
    expect(layout.textLines).toBeGreaterThan(1)
    expect(layout.cellFont).toBe(layout.messageFont)
    const output = page.getByRole('region', { name: 'Activity output' })
    expect(await output.evaluate(el => el.scrollWidth - el.clientWidth)).toBeLessThanOrEqual(1)
    expect(await page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    await expect(message.getByRole('link', { name: 'Reference', exact: true })).toHaveAttribute('href', longPath)

    if (width === 320) {
      const wideTable = message.getByRole('table').last()
      const headerLines = await wideTable.getByRole('columnheader').first().evaluate((cell) => {
        const range = document.createRange()
        range.selectNodeContents(cell)
        return range.getClientRects().length
      })
      expect(headerLines).toBeLessThanOrEqual(2)
      const scroll = await wideTable.evaluate((table) => {
        const container = table.parentElement!
        container.scrollLeft = container.scrollWidth
        return { left: container.scrollLeft, overflow: container.scrollWidth - container.clientWidth }
      })
      expect(scroll.overflow).toBeGreaterThan(0)
      expect(scroll.left).toBeGreaterThan(0)
      await expect(wideTable.getByRole('cell', { name: 'Value 12', exact: true })).toBeInViewport()
    }

    await message.scrollIntoViewIfNeeded()
    await page.screenshot({ path: test.info().outputPath(`markdown-tables-${width}.png`), animations: 'disabled' })
  }
})

for (const kind of ['chat', 'task'] as const) {
  test(`${kind} automatically fills folded history without a click or scroll`, async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
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

    // Reads fold into bounded groups; short output should still prefetch older pages.
    for (let i = 0; i < 250; i++) {
      workspace.service.store.event(run.id, 'item.completed', 'Checked a file', {
        item: {
          id: `fold-${i}`,
          type: 'command_execution',
          command: 'cat README.md',
          exit_code: 0,
        },
      })
    }

    workspace.service.store.event(run.id, 'item.completed', 'Latest short answer', { item: { id: 'latest-short', type: 'agent_message', text: 'Latest short answer' } })
    const requests: string[] = []
    page.on('request', (request) => {
      if (request.url().includes('/history?'))
        requests.push(request.url())
    })
    await page.goto(workspacePath(`${workspace.url}${path}`))
    await authenticateWorkspace(page)
    await expect(page.locator('.activity-message').filter({ hasText: 'Latest short answer' })).toBeVisible()
    await expect(page.locator('.activity-message').filter({ hasText: 'Buffered message 99:' })).toBeAttached()
    expect(requests.length).toBeGreaterThanOrEqual(2)
    expect(new Set(requests).size).toBe(requests.length)
    const summaries = await page.getByTestId('agent-actions').allTextContents()
    const readCounts = summaries.flatMap(text => [...text.matchAll(/Read (\d+) files/g)].map(match => Number(match[1])))
    expect(readCounts.length).toBeGreaterThan(1)
    expect(readCounts.every(count => count <= 30)).toBe(true)
    // Loading older pages must keep following the latest output.
    const scroller = page.getByRole('region', { name: 'Activity output' })
    await expect.poll(() => scroller.evaluate(el => el.scrollHeight - el.clientHeight - el.scrollTop)).toBeLessThan(2)
    await expectFollowing(page)
    await page.screenshot({ path: test.info().outputPath(`${kind}-automatic-history.png`), animations: 'disabled' })
  })
}

test('an older page preserves expanded newer tools and their open detail sheet', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const run = workspace.service.store.runs().find(run => run.status === 'succeeded' && run.trigger !== 'chat')!
  workspace.service.store.event(run.id, 'turn.started', '')
  for (let i = 0; i < 260; i++) {
    workspace.service.store.event(run.id, 'item.completed', '', {
      item: {
        id: `inspect-${i}`,
        type: 'command_execution',
        command: `echo inspect-${i}`,
        exit_code: 0,
      },
    })
  }

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
  try {
    await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
    await authenticateWorkspace(page)
    const groups = page.getByTestId('agent-actions')
    await expect(groups.nth(1)).toBeVisible()
    const group = groups.nth(1)
    await group.locator(':scope > button').click()
    const step = group.getByTestId('agent-step').first()
    const label = (await step.textContent())!.match(/echo inspect-\d+/)![0]
    const groupCount = await groups.count()
    const stepCount = await group.getByTestId('agent-step').count()
    await step.click()
    const sheet = page.getByTestId('agent-step-sheet')
    await expect(sheet).toBeVisible()
    // A pending automatic page can finish while the reader inspects a tool.
    await expect.poll(() => requested).toBe(true)
    release()
    await expect.poll(() => groups.count()).toBeGreaterThan(groupCount)
    await expect(sheet).toBeVisible()
    await expect(sheet).toContainText(label)
    await expect(page.getByTestId('agent-step')).toHaveCount(stepCount)
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'wait' })
  }
})

test('automatic history pauses on errors and resumes only after retry', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const run = workspace.service.store.runs().find(run => run.status === 'succeeded' && run.trigger !== 'chat')!
  for (let i = 0; i < 220; i++) {
    workspace.service.store.event(run.id, 'item.completed', 'Checked a file', {
      item: {
        id: `retry-${i}`,
        type: 'command_execution',
        command: 'cat README.md',
        exit_code: 0,
      },
    })
  }

  let requests = 0
  let fail = true
  await page.route('**/history?**', async (route) => {
    requests++
    if (fail)
      await route.fulfill({ status: 503, json: { error: 'History temporarily unavailable' } })
    else
      await route.continue()
  })
  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await authenticateWorkspace(page)
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
  await expectFollowing(page)
})

test('signing out closes live subscriptions before the session is revoked', async ({ page, workspace }) => {
  await page.goto(workspacePath(workspace.url))
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'New conversation', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'What are we building?' })).toBeVisible()
  const unauthorized: string[] = []
  page.on('response', (response) => {
    if (response.status() === 401)
      unauthorized.push(response.url())
  })
  await page.route('**/api/account/logout', async (route) => {
    const response = await route.fetch()
    // Leave the UI mounted after revocation: an open stream would immediately
    // close and trigger its access check before the logout response arrives.
    await new Promise(resolve => setTimeout(resolve, 800))
    await route.fulfill({ response })
  })
  await page.getByRole('button', { name: 'Sign out', exact: true }).click()
  await expect(page.getByLabel('Email address')).toBeVisible()
  expect(unauthorized).toEqual([])
})

test('signIn settles an authenticated root redirect before reloading', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspace.url)
  await authenticateWorkspace(page)
  await expect(page.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()

  // Keep the transient authenticated root observable before its navigation
  // begins. A stalled destination request alone makes Playwright locators wait
  // for navigation, hiding the race rather than exercising it.
  await page.addInitScript((installationSlug) => {
    if (location.pathname !== '/')
      return
    const target = window as typeof window & { releaseRootNavigation?: () => void }
    let destination = ''
    let blocked = true
    window.navigation?.addEventListener('navigate', (event) => {
      if (blocked && event.cancelable && new URL(event.destination.url).pathname === `/installations/${installationSlug}/`) {
        event.preventDefault()
        destination = event.destination.url
      }
    })
    target.releaseRootNavigation = () => {
      blocked = false
      if (destination)
        location.assign(destination)
    }
  }, workspace.installationSlug)
  await page.goto(workspace.url, { waitUntil: 'commit' })
  await expect(page.getByRole('button', { name: 'Sign out', exact: true, includeHidden: true }).first()).toBeAttached()
  const reload = page.reload.bind(page)
  page.reload = async (options) => {
    expect(new URL(page.url()).pathname, 'the fixture must settle the root redirect before reload').toBe(`/installations/${workspace.installationSlug}/`)
    return reload(options)
  }

  const authenticated = authenticateWorkspace(page).then(() => undefined, error => error)
  // A controlled navigation stall exposes the transient authenticated root;
  // this is fault injection, not extra time for a success assertion.
  await page.waitForTimeout(500)
  await page.evaluate(() => (window as typeof window & { releaseRootNavigation?: () => void }).releaseRootNavigation?.())
  expect(await authenticated).toBeUndefined()
  await expect(page).toHaveURL(new RegExp(`/installations/${workspace.installationSlug}/$`))
  await expect(page.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
})

test('two independent clients follow deltas, recover offline, refresh mid-answer and survive a server restart', async ({ page, browser, workspace }) => {
  test.setTimeout(90000)
  initializeRepository(workspace.projectPath)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const contexts: BrowserContext[] = []
  const signIn = async (target: Page) => {
    await useRelayForHttpMocks(target)
    await target.goto(workspacePath(workspace.url))
    await authenticateWorkspace(target)
    await expect(target.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
    // Reusing an authenticated page must also settle its account view safely.
    await authenticateWorkspace(target)
    await expect(target.getByRole('heading', { name: 'Fil', exact: true })).toBeVisible()
    await target.goto(workspacePath(`${workspace.url}/chats/${chat.id}`))
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
    // The beacon transport permits one finite read before its first live batch.
    // Once connected, deltas must arrive without recurring history/metadata reads.
    requests.length = 0
    await second.setOffline(true)
    await expect(message(page)).toContainText('020')
    await expect(other.getByRole('region', { name: 'Chat workspace', exact: true }).getByRole('status').filter({ hasText: /Offline|Reconnecting/ })).toBeVisible()
    await second.setOffline(false)
    await expect(message(other)).toContainText('025')
    expect(requests.some(url => /\/events\?|\/artifacts$/.test(url))).toBe(false)
    await page.reload()
    await expect(message(page)).toContainText('035')
    requests.length = 0
    await expect(message(page)).not.toContainText('100')
    await page.setViewportSize({ width: 390, height: 844 })
    await page.emulateMedia({ colorScheme: 'dark' })
    await expectSingleScroll(page)
    await page.screenshot({ path: test.info().outputPath('stream-mobile-dark.png'), animations: 'disabled' })
    await expect(message(page)).toContainText('100')
    await expect(message(other)).toContainText('100')
    await expect(message(page)).toHaveCount(1)
    await expect(message(other)).toHaveCount(1)
    // Compare streamed content: the header clock is not part of the provider output.
    expect(await message(page).locator('.markdown').textContent()).toBe(await message(other).locator('.markdown').textContent())
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
    await page.goto(workspacePath(`${workspace.url}/chats/${empty.id}`))
    await expect(message(page)).toHaveCount(0)
    await page.goto(workspacePath(`${workspace.url}/chats/${chat.id}`))
    await expect(message(page)).toHaveCount(1)
  }
  finally {
    for (const context of contexts)
      await context.close()
  }
})

test('cached history survives reload before a delayed stream, then clear on logout', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Cache persistence proof' })
  await page.goto(workspacePath(`${workspace.url}/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.locator('.activity-message').filter({ hasText: 'Cache persistence proof' }).first()).toBeVisible()
  const count = (current = false) => page.evaluate(({ chatId, current }) => new Promise<number>((resolve, reject) => {
    const request = indexedDB.open('cairn-history-v2')
    request.onsuccess = () => {
      const db = request.result
      const tx = db.transaction('entries', 'readonly')
      const read = tx.objectStore('entries').getAll()
      read.onsuccess = () => resolve(current
        ? read.result.filter(entry => entry.key.endsWith(`/chats/${chatId}/stream`) && JSON.parse(entry.text).cursor > 0).length
        : read.result.length)
      tx.oncomplete = () => db.close()
    }

    request.onerror = () => reject(request.error)
  }), { chatId: chat.id, current })
  await expect.poll(() => count(true)).toBe(1)
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
  await expect(page.getByLabel('Email address')).toBeVisible()
  // The account redirect can replace the execution context while storage is inspected.
  await expect(async () => expect(await count()).toBe(0)).toPass()
})

test('a cached run restores the reading offset while its stream is still connecting', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const run = workspace.service.store.runs()[0]
  for (let i = 0; i < 40; i++) {
    const text = `Saved reading position ${i}. A longer paragraph to exercise the scrolling activity view across reloads.`
    workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `cache-${i}`, type: 'agent_message', text } })
  }

  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Saved reading position 39.' })).toBeVisible()
  const scroller = page.getByRole('region', { name: 'Activity output' })
  // Reading older output pauses following.
  await scroller.evaluate(element => element.scrollTop = 300)
  await expect(page.getByRole('button', { name: 'Latest activity', exact: true })).toBeVisible()
  await expect.poll(() => scroller.evaluate(element => element.scrollTop)).toBe(300)
  await expect.poll(() => page.evaluate(() => new Promise<number>((resolve) => {
    const request = indexedDB.open('cairn-history-v2')
    request.onsuccess = () => {
      const db = request.result
      const tx = db.transaction('entries', 'readonly')
      const read = tx.objectStore('entries').getAll()
      read.onsuccess = () => resolve(read.result.map(entry => JSON.parse(entry.text).position?.top).find(top => top === 300) ?? -1)
      tx.oncomplete = () => db.close()
    }
  }))).toBe(300)
  // A previous visit may have cached "running" just before the run completed.
  await page.goto(workspacePath(`${workspace.url}/agents`))
  await page.evaluate(() => new Promise<void>((resolve, reject) => {
    const request = indexedDB.open('cairn-history-v2')
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
    await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
    // A run opens on its conversation, restoring the cached reading offset before the stream connects.
    // Scroll anchoring may then absorb a sub-pixel change above the reader by moving the offset.
    await expect(page.getByRole('button', { name: /^Conversation/ })).toHaveAttribute('aria-pressed', 'true')
    await expect.poll(() => scroller.evaluate(element => Math.abs(element.scrollTop - 300))).toBeLessThanOrEqual(1)
    await expect(page.getByRole('button', { name: 'Latest activity', exact: true })).toBeVisible()
  }
  finally {
    release()
    await page.unrouteAll({ behavior: 'wait' })
  }

  await expect(page.getByRole('status').filter({ hasText: 'Updating…' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /^Conversation/ })).toHaveAttribute('aria-pressed', 'true')
  // The way back resumes following and keeps keyboard focus in the reader.
  const latest = page.getByRole('button', { name: 'Latest activity', exact: true })
  await latest.focus()
  await page.keyboard.press('Enter')
  await expect(scroller).toBeFocused()
  await expectFollowing(page)
  // Finish removing the previous button before starting a fresh reading gesture.
  await expect(latest).toHaveCount(0)
  // Scrolling up pauses following again; scrolling back to the end resumes it, as on Android.
  await scroller.evaluate(element => element.scrollTop = 300)
  await expect(latest).toBeVisible()
  // A scroll while the way back holds keyboard focus keeps it there.
  await latest.focus()
  await scroller.evaluate(element => element.scrollTop -= 40)
  await expect(latest).toBeFocused()
  // Reaching the end removes it, handing focus to the reader.
  await scroller.evaluate(element => element.scrollTop = element.scrollHeight)
  await expect(latest).toHaveCount(0)
  await expect(scroller).toBeFocused()
  await expectFollowing(page)
})

test('following the latest output resumes a long backlog on the recent window, while a reader keeps its place', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const run = workspace.service.store.runs()[0]
  const write = (from: number, to: number) => {
    for (let i = from; i < to; i++) {
      const text = `Resume line ${String(i).padStart(3, '0')}. A paragraph to retain a stable reading position.`
      workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `resume-${i}`, type: 'agent_message', text } })
    }
  }

  const line = (i: number) => page.locator('.activity-message').filter({ hasText: `Resume line ${String(i).padStart(3, '0')}.` })
  const saved = () => page.evaluate(runId => new Promise<{ cursor: number, top?: number, follow?: boolean } | null>((resolve) => {
    const open = indexedDB.open('cairn-history-v2')
    open.onsuccess = () => {
      const db = open.result
      const tx = db.transaction('entries')
      const request = tx.objectStore('entries').getAll()
      request.onsuccess = () => {
        const value = request.result.map(entry => JSON.parse(entry.text)).find(entry => entry.state.run?.id === runId)
        resolve(value ? { cursor: value.cursor, top: value.position?.top, follow: value.position?.follow } : null)
      }

      tx.oncomplete = () => db.close()
    }
  }), run.id)
  const streams: string[] = []
  page.on('request', (request) => {
    if (request.url().includes(`/api/runs/${run.id}/stream?`))
      streams.push(request.url())
  })
  write(0, 40)
  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(line(39)).toBeVisible()
  await expect.poll(async () => (await saved())?.cursor ?? 0).toBeGreaterThan(0)

  // Following viewers skip the backlog: the cached lines give way to the latest window.
  await page.goto(workspacePath(`${workspace.url}/agents`))
  write(40, 300)
  streams.length = 0
  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await expect(line(299)).toBeVisible()
  expect(new URL(streams[0]).searchParams.get('recent')).toBe('1')
  await expect(line(39)).toHaveCount(0)
  await expectFollowing(page)
  const scroller = page.getByRole('region', { name: 'Activity output' })
  // Older messages load from the window's boundary.
  await expect.poll(async () => {
    await scroller.evaluate(el => el.scrollTop = 0)
    return line(150).count()
  }).toBe(1)

  // A reader of older messages keeps the replay and its place. Reading mid-history
  // stays clear of older-page prefetching, which shifts the offset to hold the content.
  await scroller.evaluate(el => el.scrollTop = Math.round(el.scrollHeight / 2))
  await expect(page.getByRole('button', { name: 'Latest activity', exact: true })).toBeVisible()
  const top = await scroller.evaluate(el => el.scrollTop)
  await expect.poll(async () => (await saved())?.top).toBe(top)
  expect((await saved())?.follow).toBe(false)
  const anchor = await scroller.evaluate((el) => {
    const edge = el.getBoundingClientRect().top
    const first = [...el.querySelectorAll('.activity-message')].find(message => message.getBoundingClientRect().top >= edge)
    return Number(first?.textContent?.match(/Resume line (\d{3})\./)?.[1])
  })
  expect(anchor).toBeGreaterThan(0)
  const anchorTop = async () => (await line(anchor).boundingBox())?.y ?? Number.NaN
  const before = await anchorTop()
  await page.goto(workspacePath(`${workspace.url}/agents`))
  write(300, 560)
  streams.length = 0
  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await expect(line(559)).toBeAttached()
  expect(new URL(streams[0]).searchParams.get('recent')).toBeNull()
  await expect.poll(async () => Math.abs(await anchorTop() - before)).toBeLessThan(2)
  await expect(page.getByRole('button', { name: 'Latest activity', exact: true })).toBeVisible()
})

test('recent history loads older pages without moving the reader and survives a blocked reconnect', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const run = workspace.service.store.runs()[0]
  for (let i = 0; i < 350; i++) {
    const text = `Paged line ${String(i).padStart(3, '0')}. A paragraph to retain a stable reading position.`
    workspace.service.store.event(run.id, 'item.completed', text, { item: { id: `page-${i}`, type: 'agent_message', text } })
  }

  await page.goto(workspacePath(`${workspace.url}/runs/${run.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 349' })).toBeVisible()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 249' })).toHaveCount(0)
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
  await useRelayForHttpMocks(page)
  // Read beyond the former 200-event cache window, then persist all loaded pages.
  await scroller.evaluate(el => el.scrollTop = 0)
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 050' })).toBeAttached()
  await scroller.evaluate(el => el.scrollTop = 0)
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 000' })).toBeAttached()
  // The run also has fixture events before line 000: load that unread prefix before
  // asserting that reopening needs no history requests at all.
  await expect.poll(async () => {
    await scroller.evaluate(el => el.scrollTop = 0)
    return page.evaluate(runId => new Promise<boolean>((resolve) => {
      const open = indexedDB.open('cairn-history-v2')
      open.onsuccess = () => {
        const db = open.result
        const tx = db.transaction('entries')
        const request = tx.objectStore('entries').getAll()
        request.onsuccess = () => resolve(request.result.some((entry) => {
          const saved = JSON.parse(entry.text)
          return saved.state.run?.id === runId && saved.events.length >= 350 && saved.hasOlder === false
        }))
        tx.oncomplete = () => db.close()
      }
    }), run.id)
  }).toBe(true)
  // Move back to the latest content and let its bounded disk snapshot commit.
  await page.getByRole('button', { name: 'Latest activity', exact: true }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 349' })).toBeVisible()
  await expect.poll(() => page.evaluate(() => new Promise<number>((resolve) => {
    const open = indexedDB.open('cairn-history-v2')
    open.onsuccess = () => {
      const db = open.result
      const tx = db.transaction('entries')
      const request = tx.objectStore('entries').getAll()
      request.onsuccess = () => resolve(Math.max(0, ...request.result.map(e => JSON.parse(e.text).events.length)))
      tx.oncomplete = () => db.close()
    }
  }))).toBeGreaterThanOrEqual(350)
  let repeatedHistoryReads = 0
  page.on('request', (request) => {
    if (request.url().includes(`/runs/${run.id}/history?`) || request.url().includes(`/runs/${run.id}/events?`))
      repeatedHistoryReads++
  })
  await page.route(`**/api/runs/${run.id}/stream?**`, route => route.abort())
  await page.reload()
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 349' })).toBeVisible()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 000' })).toBeAttached()
  await scroller.evaluate(el => el.scrollTop = 0)
  await page.waitForTimeout(750)
  expect(repeatedHistoryReads).toBe(0)
  await page.unroute(`**/api/runs/${run.id}/stream?**`)
  await page.reload()
  await page.getByRole('button', { name: /^Conversation/ }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Paged line 000' })).toBeAttached()
  await scroller.evaluate(el => el.scrollTop = 0)
  await page.waitForTimeout(750)
  expect(repeatedHistoryReads).toBe(0)
})
