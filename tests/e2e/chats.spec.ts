import type { Page } from '@playwright/test'
import type { Workspace } from './fixtures'
import type { ResumeDiagnosticRecord } from './resume-diagnostic'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectChatReady,
  expectSingleScroll,
  initializeRepository,
  isolatedWorkspaceTest as test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import { projectResumeDiagnostic, RESUME_DIAGNOSTIC_MARKER } from './resume-diagnostic'

type ResumeDiagnosticWindow = Window & {
  __cairnTestResumeDiagnostic?: (record: ResumeDiagnosticRecord) => void
  __cairnTestResumeDiagnosticRecords?: ResumeDiagnosticRecord[]
}

async function installResumeDiagnosticProbe(page: Page) {
  await page.addInitScript(() => {
    const records: ResumeDiagnosticRecord[] = []
    const diagnosticWindow = window as ResumeDiagnosticWindow
    diagnosticWindow.__cairnTestResumeDiagnosticRecords = records
    diagnosticWindow.__cairnTestResumeDiagnostic = (record) => {
      if (records.length < 16)
        records.push({ ...record })
    }
  })
}

async function reportResumeDiagnostic(page: Page, requestCount: number) {
  let report = projectResumeDiagnostic(requestCount, undefined)
  try {
    const source = await page.evaluate(() => (window as ResumeDiagnosticWindow).__cairnTestResumeDiagnosticRecords)
    report = projectResumeDiagnostic(requestCount, source)
  }
  catch {
    // Failure reporting must not replace the original request-count assertion.
  }

  try {
    await test.info().attach('resume-diagnostic.json', {
      body: JSON.stringify(report),
      contentType: 'application/json',
    })
  }
  catch {
    report = projectResumeDiagnostic(requestCount, undefined)
    try {
      await test.info().attach('resume-diagnostic.json', {
        body: JSON.stringify(report),
        contentType: 'application/json',
      })
    }
    catch {
      // Keep the fixed console record as the last available diagnostic.
    }
  }

  try {
    console.error(`${RESUME_DIAGNOSTIC_MARKER} ${JSON.stringify(report)}`)
  }
  catch {
    // Keep the original assertion as the reported failure.
  }
}

async function expectSingleResumeRequest(page: Page, requestCount: () => number) {
  try {
    await expect.poll(() => ({ requests: requestCount() })).toMatchObject({ requests: 1 })
  }
  catch (failure) {
    try {
      await reportResumeDiagnostic(page, requestCount())
    }
    catch {
      // Diagnostic reporting cannot mask the original request-count assertion.
    }

    throw failure
  }
}

async function createFailedCodexQuotaChat(
  workspace: Workspace,
  agentSettings: { model: string, reasoning: string, speed: string },
) {
  const chat = await workspace.api('/api/chats', 'POST', {})
  const turnId = randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: turnId, text: 'Keep this Codex turn and session' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  const before = workspace.service.chats.detail(chat.id).run!
  workspace.service.store.updateRun(before.id, {
    status: 'failed',
    error: 'Usage limit has been reached',
    resumeAvailable: true,
    snapshot: {
      ...before.snapshot,
      agent: {
        ...before.snapshot.agent,
        provider: 'codex',
        ...agentSettings,
      },
    },
  })
  workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, paused: true })
  workspace.service.store.event(before.id, 'turn.failed', 'Usage limit has been reached', { type: 'turn.failed' })
  workspace.service.store.event(before.id, 'status', 'failed')

  return { chat, before, turnId }
}

test('expired environments retain the conversation and explain fresh resumption and cleanup retries', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...chat, environmentState: 'expired', environmentExpiredAt: Date.now() })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const notice = page.getByRole('status').filter({ hasText: 'Working environment expired' })
  await expect(notice).toContainText('Your next message starts a fresh environment')
  await expect(notice).toContainText('published files and shared links are retained')
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()
  workspace.service.store.put('chats', {
    ...chat,
    environmentState: 'cleaning',
    environmentRetryAfter: Date.now() + 3600_000,
    environmentError: 'Node unavailable',
  })
  await page.reload()
  await expect(notice).toContainText('Please wait before sending another message')
  await expect(notice).toContainText('Cleanup will retry: Node unavailable')
})

test('a lost message acknowledgement retries the same submission without duplicating it', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const attempts: { id: string, chatId: string }[] = []
  await page.route(/\/chats\/([^/]+)\/messages$/, async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    const chatId = new URL(route.request().url()).pathname.split('/').at(-2)!
    attempts.push({ id: route.request().postDataJSON().id, chatId })
    const response = await route.fetch()
    expect(response.ok()).toBe(true)
    if (attempts.length === 1)
      return route.abort('failed')
    return route.fulfill({ response })
  })
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  const composer = page.getByRole('textbox', { name: 'Message', exact: true })
  await composer.fill('Keep this message exactly once')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.getByText('Connection unavailable. Please try again.', { exact: true })).toBeVisible()
  await expect(composer).toHaveValue('Keep this message exactly once')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  expect(attempts).toHaveLength(2)
  expect(attempts[1]).toEqual(attempts[0])
  const detail = workspace.service.chats.detail(attempts[0].chatId)
  expect(detail.messages).toHaveLength(1)
  expect(detail.messages[0].id).toBe(attempts[0].id)
})

for (const stage of ['creation', 'upload', 'message', 'message error'] as const) {
  test(`fresh conversation ignores a late ${stage} response`, async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    const pattern = stage === 'creation' ? /\/chats$/ : stage === 'upload' ? /\/chats\/[^/]+\/attachments\// : /\/chats\/[^/]+\/messages$/
    let release = () => {}
    const gate = new Promise<void>((resolve) => {
      release = resolve
    })
    let enter = () => {}
    const entered = new Promise<void>((resolve) => {
      enter = resolve
    })
    let held = false
    let oldChatId = ''
    const submitted: string[] = []
    page.on('request', (request) => {
      if (request.method() === 'POST' && /\/chats\/[^/]+\/messages$/.test(request.url()))
        submitted.push(new URL(request.url()).pathname.split('/').at(-2)!)
    })
    await page.route(pattern, async (route) => {
      if (held || !['POST', 'PUT'].includes(route.request().method()))
        return route.continue()
      held = true
      const response = await route.fetch()
      expect(response.ok()).toBe(true)
      oldChatId = stage === 'creation' ? (await response.json()).id : new URL(route.request().url()).pathname.split('/chats/').at(-1)!.split('/')[0]!
      enter()
      await gate
      if (stage === 'message error')
        return route.fulfill({ status: 503, json: { error: 'Late request failed' } })
      return route.fulfill({ response })
    })
    const completed = page.waitForResponse(response => pattern.test(response.url()) && ['POST', 'PUT'].includes(response.request().method()))
    try {
      await page.goto(workspacePath('/chats'))
      await authenticateWorkspace(page)
      const composer = page.getByRole('textbox', { name: 'Message', exact: true })
      await composer.fill('The first request is still in flight')
      if (stage === 'upload')
        await page.getByLabel('Attach files', { exact: true }).setInputFiles({ name: 'old-note.txt', mimeType: 'text/plain', buffer: Buffer.from('old note') })
      await page.getByRole('button', { name: 'Send', exact: true }).click()
      await entered
      await page.getByRole('link', { name: 'New conversation', exact: true }).click()
      await expect(composer).toHaveValue('')
      await composer.fill('Keep the replacement draft')
      if (stage === 'upload')
        await page.getByLabel('Attach files', { exact: true }).setInputFiles({ name: 'new-note.txt', mimeType: 'text/plain', buffer: Buffer.from('new note') })
      release()
      await (await completed).finished()
      await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))))
      await expect(page).toHaveURL(workspacePath('/chats'))
      await expect(composer).toHaveValue('Keep the replacement draft')
      await expect(page.getByText('Late request failed', { exact: true })).toHaveCount(0)
      if (stage === 'upload')
        await expect(page.getByText('new-note.txt', { exact: true })).toBeVisible()
      if (stage === 'creation' || stage === 'upload')
        expect(submitted).not.toContain(oldChatId)
      else
        expect(workspace.service.chats.detail(oldChatId).messages).toHaveLength(1)
    }
    finally {
      release()
    }
  })
}

test('a previous send cannot release a newer conversation submission', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  let releaseOld = () => {}
  const oldGate = new Promise<void>((resolve) => {
    releaseOld = resolve
  })
  let releaseNew = () => {}
  const newGate = new Promise<void>((resolve) => {
    releaseNew = resolve
  })
  let oldEntered = () => {}
  const oldReady = new Promise<void>((resolve) => {
    oldEntered = resolve
  })
  let newEntered = () => {}
  const newReady = new Promise<void>((resolve) => {
    newEntered = resolve
  })
  const targets: string[] = []
  await page.route(/\/chats\/[^/]+\/messages$/, async (route) => {
    if (route.request().method() !== 'POST')
      return route.continue()
    const index = targets.length
    targets.push(new URL(route.request().url()).pathname.split('/').at(-2)!)
    const response = await route.fetch()
    expect(response.ok()).toBe(true)
    if (index === 0) {
      oldEntered()
      await oldGate
    }
    else {
      newEntered()
      await newGate
    }

    return route.fulfill({ response })
  })
  try {
    await page.goto(workspacePath('/chats'))
    await authenticateWorkspace(page)
    const composer = page.getByRole('textbox', { name: 'Message', exact: true })
    await composer.fill('The previous request is still pending')
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await oldReady
    await page.getByRole('link', { name: 'New conversation', exact: true }).click()
    await composer.fill('The new request owns this conversation')
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await newReady
    expect(targets).toHaveLength(2)
    expect(targets[1]).not.toBe(targets[0])
    const oldResponse = page.waitForResponse(response => response.request().method() === 'POST' && response.url().endsWith(`/chats/${targets[0]}/messages`))
    releaseOld()
    await (await oldResponse).finished()
    await page.evaluate(() => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))))
    await expect(page).toHaveURL(workspacePath('/chats'))
    await expect(page.getByRole('button', { name: 'Sending…', exact: true })).toBeDisabled()
    releaseNew()
    const destination = workspace.service.chats.detail(targets[1]!)
    await expect(page).toHaveURL(workspacePath(`/chats/${destination.slug}`))
    await expect(page.locator('.activity-message').getByText('The new request owns this conversation', { exact: true })).toBeVisible()
  }
  finally {
    releaseOld()
    releaseNew()
  }
})

for (const entry of ['command', 'workspace link', 'search draft'] as const) {
  test(`new conversation through ${entry} clears an uncertain submission with identical content`, async ({ page, workspace }) => {
    await useRelayForHttpMocks(page)
    const attempts: { id: string, chatId: string }[] = []
    const messagePath = /\/chats\/[^/]+\/messages$/
    await page.route(messagePath, async (route) => {
      if (route.request().method() !== 'POST')
        return route.continue()
      attempts.push({
        id: route.request().postDataJSON().id,
        chatId: new URL(route.request().url()).pathname.split('/').at(-2)!,
      })
      const response = await route.fetch()
      if (attempts.length === 1) {
        expect(response.ok()).toBe(true)
        return route.abort('failed')
      }

      return route.fulfill({ response })
    })
    await page.goto(workspacePath('/chats'))
    await authenticateWorkspace(page)
    const composer = page.getByRole('textbox', { name: 'Message', exact: true })
    await composer.fill('Same content, separate conversation')
    const lostAcknowledgement = page.waitForEvent('requestfailed', request => request.method() === 'POST' && messagePath.test(request.url()))
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await lostAcknowledgement
    await expect(page.getByText('Connection unavailable. Please try again.', { exact: true })).toBeVisible()
    await expect(page).toHaveURL(workspacePath('/chats'))
    if (entry === 'workspace link') {
      await page.getByRole('link', { name: 'New conversation', exact: true }).click()
    }
    else {
      if (entry === 'search draft')
        await page.getByRole('tab', { name: 'Tout', exact: true }).click()
      await page.keyboard.press('Control+k')
      const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
      if (entry === 'search draft') {
        await palette.getByRole('combobox', { name: 'Search', exact: true }).fill('Same content, separate conversation')
        const firstChat = attempts[0]!.chatId
        await expect.poll(() => workspace.service.chats.detail(firstChat).run?.status).toBe('succeeded')
        const decoy = await workspace.api('/api/chats', 'POST', {})
        await workspace.api(`/api/chats/${decoy.id}`, 'PUT', { title: 'Same content, separate conversation decoy', revision: decoy.revision })
        const decoyOption = palette.getByRole('option', { name: 'Same content, separate conversation decoy Main agent', exact: true })
        await expect(decoyOption).toBeVisible()
        const filDecoy = page.getByRole('button', { name: 'Open “Same content, separate conversation decoy”', exact: true, includeHidden: true })
        await expect(filDecoy).toHaveCount(1)
        const start = palette.getByRole('option', { name: /^Start a conversation:/ })
        await start.hover()
        await page.mouse.down()
        await workspace.api(`/api/chats/${decoy.id}`, 'DELETE', {})
        // The Fil observes the same live list even while the modal is open.
        await expect(filDecoy).toHaveCount(0)
        await page.mouse.up()
      }
      else {
        await palette.getByRole('option', { name: 'New conversation C', exact: true }).click()
      }
    }

    await expect(composer).toHaveValue(entry === 'search draft' ? 'Same content, separate conversation' : '')
    await composer.fill('Same content, separate conversation')
    await page.getByRole('button', { name: 'Send', exact: true }).click()
    await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
    expect(attempts).toHaveLength(2)
    expect(attempts[1].chatId).not.toBe(attempts[0].chatId)
    expect(attempts[1].id).not.toBe(attempts[0].id)
    for (const attempt of attempts) {
      const detail = workspace.service.chats.detail(attempt.chatId)
      expect(detail.messages).toHaveLength(1)
      expect(detail.messages[0].id).toBe(attempt.id)
    }
  })
}

test('workspace search keeps pointer cancellation and keyboard selection after a live update', async ({ page, workspace }) => {
  const decoy = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${decoy.id}`, 'PUT', { title: 'Pointer cancellation decoy', revision: decoy.revision })
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  const composer = page.getByRole('textbox', { name: 'Message', exact: true })
  await composer.fill('Keep this draft until I choose')
  await page.getByRole('tab', { name: 'Tout', exact: true }).click()
  const filDecoy = page.getByRole('button', { name: 'Open “Pointer cancellation decoy”', exact: true, includeHidden: true })
  await expect(filDecoy).toHaveCount(1)
  await page.keyboard.press('Control+k')
  const palette = page.getByRole('dialog', { name: 'Search and commands', exact: true })
  const search = palette.getByRole('combobox', { name: 'Search', exact: true })
  await search.fill('Pointer cancellation')
  const decoyOption = palette.getByRole('option', { name: 'Pointer cancellation decoy Main agent', exact: true })
  await expect(decoyOption).toBeVisible()
  const start = palette.getByRole('option', { name: /^Start a conversation:/ })
  await start.hover()
  await page.mouse.down()
  await workspace.api(`/api/chats/${decoy.id}`, 'DELETE', {})
  await expect(filDecoy).toHaveCount(0)
  await palette.getByRole('listbox').hover({ position: { x: 1, y: 1 } })
  await page.mouse.up()
  await expect(palette).toBeVisible()
  await expect(composer).toHaveValue('Keep this draft until I choose')
  await expect(decoyOption).toHaveCount(0)
  await search.press('ArrowDown')
  await search.press('Enter')
  await expect(palette).toBeHidden()
  await expect(composer).toHaveValue('Pointer cancellation')
})

test('new conversation on the same route clears the abandoned attempt’s error', async ({ page }) => {
  await useRelayForHttpMocks(page)
  await page.route(/\/chats\/[^/]+\/messages$/, route => route.request().method() === 'POST'
    ? route.fulfill({ status: 503, json: { error: 'Simulated send failure' } })
    : route.continue())
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  const composer = page.getByRole('textbox', { name: 'Message', exact: true })
  await composer.fill('Refused first message')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  const failure = page.getByText('Simulated send failure', { exact: true })
  await expect(failure).toBeVisible()
  await page.getByRole('link', { name: 'New conversation', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/chats'))
  await expect(composer).toHaveValue('')
  await expect(failure).toBeHidden()
})

test('identical queued-message edits keep retry identity scoped to the edited message', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/pause`, 'POST', { paused: true })
  const first = randomUUID()
  const second = randomUUID()
  for (const id of [first, second])
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id, text: 'Identical queued draft' })
  const attempts: { id: string, target: string }[] = []
  await page.route(/\/chats\/[^/]+\/messages\/[^/]+$/, async (route) => {
    if (route.request().method() !== 'PUT')
      return route.continue()
    attempts.push({ id: route.request().postDataJSON().id, target: new URL(route.request().url()).pathname.split('/').at(-1)! })
    return route.fulfill({ status: 503, json: { error: 'Simulated edit failure' } })
  })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const save = page.getByRole('button', { name: 'Save', exact: true })
  await page.getByRole('button', { name: /^Queued message options/ }).first().click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await save.click()
  await expect(page.getByText('Simulated edit failure', { exact: true })).toBeVisible()
  await save.click()
  await expect(save).toBeEnabled()
  expect(attempts).toHaveLength(2)
  expect(attempts[1]).toEqual(attempts[0])
  await page.getByRole('button', { name: '+ 1 other message', exact: true }).click()
  await page.getByRole('button', { name: /^Queued message options/ }).nth(1).click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await save.click()
  await expect(save).toBeEnabled()
  expect(attempts).toHaveLength(3)
  expect(attempts[0].target).toBe(first)
  expect(attempts[2].target).toBe(second)
  expect(attempts[2].id).not.toBe(attempts[0].id)
})

test('a transient agent error shows a retry countdown and resumes without user input', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:agent-failures:[{"codexErrorInfo":"other","message":"workspace routing discovery timed out"}]',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.retry?.attempt).toBe(1)
  const waiting = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(waiting.status).toBe('queued')
  waiting.retry.nextAttemptAt = Date.now() + 60_000
  workspace.service.store.updateRun(waiting.id, { retry: waiting.retry })
  // A retry keeps earlier conversation events, including an interrupted background wait.
  workspace.service.store.event(waiting.id, 'turn.waiting', '', {
    tasks: [{ id: 'old-build', description: 'Earlier background build' }],
  })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const notice = page.getByTestId('recovery-notice')
  await expect(notice).toBeVisible()
  // The failed attempt shows one card, marked as a temporary error Cairn retries itself.
  const failure = page.getByRole('note').filter({ hasText: 'workspace routing discovery timed out' })
  await expect(failure).toHaveCount(1)
  await expect(failure).toContainText('Temporary error — automatic retry 1/5')
  await expect(page.locator('.agent-waiting')).toHaveCount(0)
  await expect(page.getByText('Background task', { exact: true })).toHaveCount(0)
  const before = await notice.textContent()
  await expect.poll(() => notice.textContent()).not.toBe(before)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1280, 390, 320]) {
      await page.setViewportSize({ width, height: 900 })
      await expect(notice).toBeVisible()
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
      await page.screenshot({ path: test.info().outputPath(`agent-retry-${colorScheme}-${width}.png`) })
    }
  }

  expect((await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(false)
  workspace.service.store.updateRun(waiting.id, { retry: { ...waiting.retry, nextAttemptAt: Date.now() } })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  await expect(page.getByText('Ready for the next step.', { exact: false }).last()).toBeVisible()
  await expect(notice).toHaveCount(0)
  await expect(failure).toHaveCount(1)
})

test('cancelling automatic recovery pauses the conversation and preserves its work', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:agent-failures:[{"code":"server_overloaded","message":"Please try again"}]',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.retry?.attempt).toBe(1)
  const waiting = (await workspace.api(`/api/chats/${chat.id}`)).run
  workspace.service.store.updateRun(waiting.id, { retry: { ...waiting.retry, nextAttemptAt: Date.now() + 60_000 } })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const notice = page.getByTestId('recovery-notice')
  await expect(notice.getByText('Retry 1 of 5')).toBeVisible()
  await notice.getByText('What happened').click()
  await expect(notice.getByText('Please try again')).toBeVisible()
  await notice.getByRole('button', { name: 'Cancel retry' }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(true)
  await expect(notice).toHaveCount(0)
  const stopped = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(stopped.id).toBe(waiting.id)
  expect(stopped.workspace).toBe(waiting.workspace)
  expect(stopped.retry).toBeNull()
  expect(stopped.status).toBe('cancelled')
})

test('stopping a chat during native cleanup keeps the later message queued', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'Finish the current reply',
  })
  await expect.poll(() => {
    const run = workspace.service.chats.detail(chat.id).run
    return run && { status: run.status, recoveryPending: run.recoveryPending }
  }).toEqual({ status: 'succeeded', recoveryPending: false })
  const completed = workspace.service.chats.detail(chat.id).run!
  workspace.service.store.deferNativeCleanup(completed.id)
  const closing = workspace.service.chats.detail(chat.id).run
  expect(closing?.executionBackend).toBe('firecracker')
  expect(closing?.nativeCleanupPending).toBe(true)
  const following = randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: following, text: 'Keep this for the next turn' })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Stop response' }).click()

  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('cancelled')
  const stopped = workspace.service.chats.detail(chat.id)
  expect(stopped.paused).toBe(true)
  expect(stopped.run?.finishedAt).toBe(completed.finishedAt)
  expect(stopped.run?.sessionId).toBe(completed.sessionId)
  expect(workspace.service.store.chatMessages(chat.id).find(message => message.id === following)?.status).toBe('queued')
})

test('native Codex reconnect notices remain visible until work continues', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:codex-reconnect fixture:chat-hang',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('running')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await expect(page.getByText('Connection interrupted', { exact: true })).toBeVisible()
    await expect(page.getByText('Reconnecting... 2/5', { exact: true })).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
  }

  await expect(page.getByText('Ignored retry notification', { exact: true })).toHaveCount(0)
  const active = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(active.retry ?? null).toBeNull()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'finish now', mode: 'steer' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const completed = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(completed.id).toBe(active.id)
  expect(completed.sessionId).toBe(active.sessionId)
  expect(completed.retry).toBeNull()
  await expect(page.getByText('Connection interrupted', { exact: true })).toHaveCount(0)
})

test('queued Claude conversation explains the missing account and continues after sign-in', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Keep this request while connecting', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountRequired).toBe('claude')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.accountWaitReason).toBe('Connect a Claude Code account in Connections before running this agent.')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await page.setViewportSize({ width: 390, height: 844 })
  await expect(page.getByText('Claude Code account needed', { exact: true })).toBeVisible({ timeout: 30000 })
  await expect(page.getByText('Waiting for a Claude Code account', { exact: true })).toBeVisible()
  await expect(page.getByText('Your agent is working…', { exact: true })).toHaveCount(0)
  await expect(page.getByText('Starting agent…', { exact: true })).toHaveCount(0)
  await page.getByRole('link', { name: 'Open Connections', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/connections'))
  await page.getByRole('button', { name: 'Add Claude Code account', exact: true }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Continue to sign-in' }).click()
  const dialog = page.getByRole('dialog', { name: 'Connect your Claude account' })
  await dialog.getByLabel('Claude authorization code').fill('fixture-code')
  await dialog.getByRole('button', { name: 'Finish sign-in' }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.accountRequired).toBeNull()
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await expect(page.getByText('Claude fixture completed', { exact: true })).toBeVisible()
  await expect(page.getByText('Claude Code account needed', { exact: true })).toHaveCount(0)
  await expect(page.locator('.activity-message').getByText('Keep this request while connecting', { exact: true })).toHaveCount(1)
})

test('same-provider quota resume submits the saved turn with the picker-applied speed', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const turnId = randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: turnId, text: 'Keep this Codex turn and session' })
  await expect.poll(() => workspace.service.chats.detail(chat.id).run?.status).toBe('succeeded')
  const before = workspace.service.chats.detail(chat.id).run!
  const savedAgent = before.snapshot.agent
  workspace.service.store.updateRun(before.id, {
    status: 'failed',
    error: 'Usage limit has been reached',
    resumeAvailable: true,
    snapshot: {
      ...before.snapshot,
      agent: {
        ...savedAgent,
        provider: 'codex',
        model: 'fixture-fast',
        reasoning: 'low',
        speed: 'priority',
      },
    },
  })
  workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, paused: true })
  workspace.service.store.event(before.id, 'turn.failed', 'Usage limit has been reached', { type: 'turn.failed' })
  workspace.service.store.event(before.id, 'status', 'failed')

  await page.route('**/codex/models', route => route.fulfill({
    json: {
      models: [
        {
          model: 'fixture-fast',
          displayName: 'Quick coder',
          description: 'Fast everyday coding',
          hidden: false,
          isDefault: true,
          defaultReasoningEffort: 'low',
          supportedReasoningEfforts: [{ reasoningEffort: 'low', description: '' }],
          routes: [],
          serviceTiers: [{ id: 'priority', name: 'Priority' }],
          defaultServiceTier: null,
          resolvedModel: null,
        },
        {
          model: 'fixture-deep',
          displayName: 'Deep thinker',
          description: 'Complex investigations',
          hidden: false,
          isDefault: false,
          defaultReasoningEffort: 'high',
          supportedReasoningEfforts: [
            { reasoningEffort: 'high', description: '' },
            { reasoningEffort: 'ultra', description: '' },
          ],
          routes: [],
          serviceTiers: [],
          defaultServiceTier: null,
          resolvedModel: null,
        },
      ],
      checkedAt: Date.now(),
      stale: false,
      error: '',
    },
  }))
  const resumeRequests: Record<string, unknown>[] = []
  await installResumeDiagnosticProbe(page)
  await page.route(`**/chats/${chat.id}/resume`, async (route) => {
    expect(route.request().method()).toBe('POST')
    resumeRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...before, status: 'queued', resumeAvailable: false } })
  })

  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const card = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(card).toBeVisible()
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await picker.click()
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  const inheritedModel = menu.getByRole('radio', { name: 'Conversation model', exact: true })
  await expect(inheritedModel).toHaveAttribute('aria-checked', 'true')
  await expect(inheritedModel).toContainText('Quick coder')
  const selectedModel = menu.getByRole('radio', { name: 'Deep thinker', exact: true })
  await selectedModel.click()
  await expect(selectedModel).toHaveAttribute('aria-checked', 'true')
  await menu.getByRole('radio', { name: 'Ultra', exact: true }).click()
  await expect(menu.getByRole('radiogroup', { name: 'Speed' })).toContainText('Standard')
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(picker).toHaveAccessibleName(/Deep thinker · Ultra reasoning/)
  await card.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => resumeRequests.length)

  expect(resumeRequests[0]).toMatchObject({
    chatId: chat.id,
    runId: before.id,
    turnId,
    selection: { model: 'fixture-deep', reasoning: 'ultra', speed: 'standard' },
  })
  expect(workspace.service.store.chatMessages(chat.id).map(message => message.id)).toContain(turnId)
})

test('same-provider quota resume retries a prelaunch turn without selection', async ({ page, workspace }) => {
  const failed = await createFailedCodexQuotaChat(workspace, {
    model: '',
    reasoning: '',
    speed: '',
  })
  workspace.service.store.updateRun(failed.before.id, { resumeAvailable: false })
  await useRelayForHttpMocks(page)

  const requests: Record<string, unknown>[] = []
  await page.route(`**/chats/${failed.chat.id}/resume`, async (route) => {
    requests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...failed.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${failed.chat.id}/pause`, async (route) => {
    await route.fulfill({ json: { paused: false } })
  })

  await page.goto(workspacePath(`/chats/${failed.chat.id}`))
  await authenticateWorkspace(page)
  const card = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(card).toBeVisible()
  await card.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => requests.length)

  expect(requests[0]).toMatchObject({
    chatId: failed.chat.id,
    runId: failed.before.id,
    turnId: failed.turnId,
  })
  expect(requests[0]).not.toHaveProperty('selection')
})

test('same-provider quota resume preserves an inherited blank model', async ({ page, workspace }) => {
  const unchanged = await createFailedCodexQuotaChat(workspace, {
    model: '',
    reasoning: '',
    speed: '',
  })
  const speedOnly = await createFailedCodexQuotaChat(workspace, {
    model: '',
    reasoning: '',
    speed: 'priority',
  })
  const reasoningOnly = await createFailedCodexQuotaChat(workspace, {
    model: '',
    reasoning: '',
    speed: '',
  })
  await useRelayForHttpMocks(page)
  await page.route('**/codex/models', route => route.fulfill({
    json: {
      models: [
        {
          model: 'fixture-fast',
          displayName: 'Quick coder',
          description: 'Fast everyday coding',
          hidden: false,
          isDefault: true,
          defaultReasoningEffort: 'low',
          supportedReasoningEfforts: [
            { reasoningEffort: 'low', description: '' },
            { reasoningEffort: 'high', description: '' },
          ],
          routes: [],
          serviceTiers: [{ id: 'priority', name: 'Priority' }],
          defaultServiceTier: null,
          resolvedModel: null,
        },
      ],
      checkedAt: Date.now(),
      stale: false,
      error: '',
    },
  }))

  const requests = new Map<string, Record<string, unknown>[]>()
  for (const saved of [unchanged, speedOnly, reasoningOnly]) {
    const body: Record<string, unknown>[] = []
    requests.set(saved.chat.id, body)
    await page.route(`**/chats/${saved.chat.id}/resume`, async (route) => {
      body.push(route.request().postDataJSON())
      await route.fulfill({ json: { ...saved.before, status: 'queued', resumeAvailable: false } })
    })
  }

  await page.goto(workspacePath(`/chats/${unchanged.chat.id}`))
  await authenticateWorkspace(page)
  const unchangedCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(unchangedCard).toBeVisible()
  await unchangedCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => requests.get(unchanged.chat.id)!.length)
  expect(requests.get(unchanged.chat.id)![0]).not.toHaveProperty('selection')

  await page.goto(workspacePath(`/chats/${speedOnly.chat.id}`))
  await authenticateWorkspace(page)
  const speedCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(speedCard).toBeVisible()
  await page.getByRole('button', { name: /^Agent, model, reasoning and speed/ }).click()
  const speedMenu = page.getByRole('dialog', { name: 'Agent & model' })
  const inheritedModel = speedMenu.getByRole('radiogroup', { name: 'Model' })
    .getByRole('radio', { name: 'Conversation model', exact: true })
  await expect(inheritedModel).toHaveAttribute('aria-checked', 'true')
  await expect(inheritedModel).toContainText('Quick coder')
  await speedMenu.getByRole('radiogroup', { name: 'Speed' })
    .getByRole('radio', { name: 'Standard', exact: true })
    .click()
  await speedMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await speedCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => requests.get(speedOnly.chat.id)!.length)
  expect(requests.get(speedOnly.chat.id)![0]).toMatchObject({
    chatId: speedOnly.chat.id,
    runId: speedOnly.before.id,
    turnId: speedOnly.turnId,
    selection: {
      model: '',
      preserveModel: true,
      reasoning: '',
      speed: 'standard',
    },
  })

  await page.goto(workspacePath(`/chats/${reasoningOnly.chat.id}`))
  await authenticateWorkspace(page)
  const reasoningCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(reasoningCard).toBeVisible()
  await page.getByRole('button', { name: /^Agent, model, reasoning and speed/ }).click()
  const reasoningMenu = page.getByRole('dialog', { name: 'Agent & model' })
  await reasoningMenu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'High', exact: true })
    .click()
  await reasoningMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await reasoningCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => requests.get(reasoningOnly.chat.id)!.length)
  expect(requests.get(reasoningOnly.chat.id)![0]).toMatchObject({
    chatId: reasoningOnly.chat.id,
    runId: reasoningOnly.before.id,
    turnId: reasoningOnly.turnId,
    selection: {
      model: '',
      preserveModel: true,
      reasoning: 'high',
      speed: 'standard',
    },
  })
})

test('same-provider quota resume submits the changed model default and unchanged saved effort', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const changed = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: 'low',
    speed: 'priority',
  })
  const unchanged = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: 'high',
    speed: 'priority',
  })
  const inheritedNoChange = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: '',
    speed: '',
  })
  const inheritedSpeedOnly = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: '',
    speed: 'priority',
  })
  const sameModelDefault = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: 'high',
    speed: 'priority',
  })
  const sameModelEffort = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: 'high',
    speed: '',
  })
  const resetDefaultIntent = await createFailedCodexQuotaChat(workspace, {
    model: 'fixture-fast',
    reasoning: 'high',
    speed: '',
  })
  await page.route('**/codex/models', route => route.fulfill({
    json: {
      models: [
        {
          model: 'fixture-fast',
          displayName: 'Quick coder',
          description: 'Fast everyday coding',
          hidden: false,
          isDefault: true,
          defaultReasoningEffort: 'low',
          supportedReasoningEfforts: [
            { reasoningEffort: 'low', description: '' },
            { reasoningEffort: 'high', description: '' },
          ],
          routes: [],
          serviceTiers: [{ id: 'priority', name: 'Priority' }],
          defaultServiceTier: null,
          resolvedModel: null,
        },
        {
          model: 'fixture-deep',
          displayName: 'Deep thinker',
          description: 'Complex investigations',
          hidden: false,
          isDefault: false,
          defaultReasoningEffort: 'high',
          supportedReasoningEfforts: [
            { reasoningEffort: 'low', description: '' },
            { reasoningEffort: 'high', description: '' },
          ],
          routes: [],
          serviceTiers: [],
          defaultServiceTier: null,
          resolvedModel: null,
        },
      ],
      checkedAt: Date.now(),
      stale: false,
      error: '',
    },
  }))
  const changedRequests: Record<string, unknown>[] = []
  const unchangedRequests: Record<string, unknown>[] = []
  const inheritedNoChangeRequests: Record<string, unknown>[] = []
  const inheritedSpeedRequests: Record<string, unknown>[] = []
  const sameModelDefaultRequests: Record<string, unknown>[] = []
  const sameModelEffortRequests: Record<string, unknown>[] = []
  const resetDefaultRequests: Record<string, unknown>[] = []
  await installResumeDiagnosticProbe(page)
  await page.route(`**/chats/${changed.chat.id}/resume`, async (route) => {
    changedRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...changed.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${unchanged.chat.id}/resume`, async (route) => {
    unchangedRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...unchanged.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${inheritedNoChange.chat.id}/resume`, async (route) => {
    inheritedNoChangeRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...inheritedNoChange.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${inheritedSpeedOnly.chat.id}/resume`, async (route) => {
    inheritedSpeedRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...inheritedSpeedOnly.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${sameModelDefault.chat.id}/resume`, async (route) => {
    sameModelDefaultRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...sameModelDefault.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${sameModelEffort.chat.id}/resume`, async (route) => {
    sameModelEffortRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...sameModelEffort.before, status: 'queued', resumeAvailable: false } })
  })
  await page.route(`**/chats/${resetDefaultIntent.chat.id}/resume`, async (route) => {
    resetDefaultRequests.push(route.request().postDataJSON())
    await route.fulfill({ json: { ...resetDefaultIntent.before, status: 'queued', resumeAvailable: false } })
  })

  await page.goto(workspacePath(`/chats/${changed.chat.id}`))
  await authenticateWorkspace(page)
  const changedCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(changedCard).toBeVisible()
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await picker.click()
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  const selectedModel = menu.getByRole('radio', { name: 'Deep thinker', exact: true })
  await selectedModel.click()
  const changedDefault = menu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'Model default · High', exact: true })
  await expect(changedDefault).toHaveAttribute('aria-checked', 'true')
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(picker).toHaveAccessibleName(/Deep thinker · High reasoning/)
  await changedCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => changedRequests.length)
  expect(changedRequests[0]).toMatchObject({
    chatId: changed.chat.id,
    runId: changed.before.id,
    turnId: changed.turnId,
    selection: { model: 'fixture-deep', reasoning: 'high', speed: 'standard' },
  })

  await page.goto(workspacePath(`/chats/${unchanged.chat.id}`))
  const unchangedCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(unchangedCard).toBeVisible()
  const inheritedPicker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await expect(inheritedPicker).toHaveAccessibleName(/Quick coder · High reasoning/)
  await inheritedPicker.click()
  const inheritedMenu = page.getByRole('dialog', { name: 'Agent & model' })
  const savedDefault = inheritedMenu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'Conversation default · High', exact: true })
  await expect(savedDefault).toHaveAttribute('aria-checked', 'true')
  await inheritedMenu.getByRole('radiogroup', { name: 'Speed' }).getByRole('radio', { name: 'Standard', exact: true }).click()
  await inheritedMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(inheritedPicker).toHaveAccessibleName(/Quick coder · High reasoning/)
  await unchangedCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => unchangedRequests.length)
  expect(unchangedRequests[0]).toMatchObject({
    chatId: unchanged.chat.id,
    runId: unchanged.before.id,
    turnId: unchanged.turnId,
    selection: { model: 'fixture-fast', reasoning: 'high', speed: 'standard' },
  })

  await page.goto(workspacePath(`/chats/${inheritedNoChange.chat.id}`))
  const inheritedNoChangeCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(inheritedNoChangeCard).toBeVisible()
  const inheritedNoChangePicker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await expect(inheritedNoChangePicker).toHaveAccessibleName(/Quick coder · Low reasoning/)
  await inheritedNoChangeCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => inheritedNoChangeRequests.length)
  expect(inheritedNoChangeRequests[0]).toMatchObject({
    chatId: inheritedNoChange.chat.id,
    runId: inheritedNoChange.before.id,
    turnId: inheritedNoChange.turnId,
  })
  expect(inheritedNoChangeRequests[0]).not.toHaveProperty('selection')

  await page.goto(workspacePath(`/chats/${inheritedSpeedOnly.chat.id}`))
  const inheritedSpeedCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(inheritedSpeedCard).toBeVisible()
  const inheritedSpeedPicker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await expect(inheritedSpeedPicker).toHaveAccessibleName(/Quick coder · Low reasoning/)
  await inheritedSpeedPicker.click()
  const inheritedSpeedMenu = page.getByRole('dialog', { name: 'Agent & model' })
  const inheritedEffort = inheritedSpeedMenu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'Conversation default · Low', exact: true })
  await expect(inheritedEffort).toHaveAttribute('aria-checked', 'true')
  await inheritedSpeedMenu.getByRole('radiogroup', { name: 'Speed' }).getByRole('radio', { name: 'Standard', exact: true }).click()
  await expect(inheritedEffort).toHaveAttribute('aria-checked', 'true')
  await inheritedSpeedMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(inheritedSpeedPicker).toHaveAccessibleName(/Quick coder · Low reasoning/)
  await inheritedSpeedCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => inheritedSpeedRequests.length)
  expect(inheritedSpeedRequests[0]).toMatchObject({
    chatId: inheritedSpeedOnly.chat.id,
    runId: inheritedSpeedOnly.before.id,
    turnId: inheritedSpeedOnly.turnId,
    selection: { model: 'fixture-fast', reasoning: '', speed: 'standard' },
  })

  await page.goto(workspacePath(`/chats/${sameModelDefault.chat.id}`))
  const sameModelDefaultCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(sameModelDefaultCard).toBeVisible()
  const sameModelDefaultPicker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await sameModelDefaultPicker.click()
  const sameModelDefaultMenu = page.getByRole('dialog', { name: 'Agent & model' })
  await sameModelDefaultMenu.getByRole('radio', { name: 'Quick coder', exact: true }).click()
  const explicitModelDefault = sameModelDefaultMenu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'Model default · Low', exact: true })
  await explicitModelDefault.click()
  await expect(explicitModelDefault).toHaveAttribute('aria-checked', 'true')
  await sameModelDefaultMenu.getByRole('radiogroup', { name: 'Speed' })
    .getByRole('radio', { name: 'Standard', exact: true })
    .click()
  await expect(explicitModelDefault).toHaveAttribute('aria-checked', 'true')
  await sameModelDefaultMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await sameModelDefaultCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => sameModelDefaultRequests.length)
  expect(sameModelDefaultRequests[0]).toMatchObject({
    chatId: sameModelDefault.chat.id,
    runId: sameModelDefault.before.id,
    turnId: sameModelDefault.turnId,
    selection: { model: 'fixture-fast', reasoning: 'low', speed: 'standard' },
  })

  await page.goto(workspacePath(`/chats/${sameModelEffort.chat.id}`))
  const sameModelEffortCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(sameModelEffortCard).toBeVisible()
  await page.getByRole('button', { name: /^Agent, model, reasoning and speed/ }).click()
  const sameModelEffortMenu = page.getByRole('dialog', { name: 'Agent & model' })
  await sameModelEffortMenu.getByRole('radio', { name: 'Quick coder', exact: true }).click()
  await sameModelEffortMenu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'Low', exact: true })
    .click()
  await sameModelEffortMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await sameModelEffortCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => sameModelEffortRequests.length)
  expect(sameModelEffortRequests[0]).toMatchObject({
    chatId: sameModelEffort.chat.id,
    runId: sameModelEffort.before.id,
    turnId: sameModelEffort.turnId,
    selection: { model: 'fixture-fast', reasoning: 'low', speed: 'standard' },
  })

  await page.goto(workspacePath(`/chats/${resetDefaultIntent.chat.id}`))
  const resetDefaultCard = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(resetDefaultCard).toBeVisible()
  await page.getByRole('button', { name: /^Agent, model, reasoning and speed/ }).click()
  const resetDefaultMenu = page.getByRole('dialog', { name: 'Agent & model' })
  await resetDefaultMenu.getByRole('radio', { name: 'Quick coder', exact: true }).click()
  await resetDefaultMenu.getByRole('radiogroup', { name: 'Reasoning' })
    .getByRole('radio', { name: 'Model default · Low', exact: true })
    .click()
  await resetDefaultMenu.getByRole('radio', { name: 'Conversation model', exact: true }).click()
  await resetDefaultMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await resetDefaultCard.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expectSingleResumeRequest(page, () => resetDefaultRequests.length)
  expect(resetDefaultRequests[0]).toMatchObject({
    chatId: resetDefaultIntent.chat.id,
    runId: resetDefaultIntent.before.id,
    turnId: resetDefaultIntent.turnId,
  })
  expect(resetDefaultRequests[0]).not.toHaveProperty('selection')
})

// Each Chromium and WebKit journey starts with its own account read budget.
test('starts project chats, steers, edits the queue and preserves a compact mobile composer', async ({ page, workspace }) => {
  initializeRepository(workspace.projectPath)
  await page.goto(workspacePath('/projects'))
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'Start chat' }).first().click()
  await expect(page.getByRole('group', { name: 'Agent', exact: true }).getByRole('radio', { name: 'Main agent', exact: true })).toBeChecked()
  await expect(page.getByRole('group', { name: 'Project', exact: true }).getByRole('radio', { name: 'Design system', exact: true })).toBeChecked()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('chat-new-desktop-light.png'), animations: 'disabled' })
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Review our component architecture. fixture:chat-hang')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  // The header stays quiet while the agent works: the avatar's ring says it.
  await expect(page.locator('.chat-header .avatar-orbit').filter({ visible: true })).toHaveCount(1)
  await expect(page.getByTestId('agent-actions').filter({ hasText: 'Followed the run' })).toHaveCount(0)
  await expect(page.locator('.activity-message').filter({ hasText: 'Review our component architecture' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Add a regression test for the composer')
  await page.getByRole('button', { name: 'Send after this turn', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.getByRole('button', { name: /^Queued message options/ }).first().click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Add a regression test for keyboard navigation')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  // Saving an edit while the agent works keeps the message queued for the next turn.
  await expect(page.getByTestId('queued-message')).toContainText('Add a regression test for keyboard navigation')
  await expect.poll(() => workspace.service.chats.detail(workspace.service.store.list('chats')[0].id).messages.find(message => message.text === 'Add a regression test for keyboard navigation')?.mode).toBe('queue')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Focus on accessibility first.')
  await page.getByRole('button', { name: 'Send now' }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Focus on accessibility first.' }).first()).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Consider the mobile experience too')
  await page.setViewportSize({ width: 390, height: 844 })
  // Phones show work the same way: the ring on the header's avatar.
  await expect(page.locator('.chat-header .avatar-orbit').filter({ visible: true })).toHaveCount(1)
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Send after this turn', exact: true })).toBeInViewport()
  await page.screenshot({ path: test.info().outputPath('chat-mobile-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ path: test.info().outputPath('chat-mobile-light.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 600 })
  await expect(page.getByRole('button', { name: 'Send after this turn', exact: true })).toBeInViewport()
  await page.emulateMedia({ colorScheme: 'dark' })
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Send now' })).toBeInViewport()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.screenshot({ path: test.info().outputPath('chat-desktop-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ path: test.info().outputPath('chat-desktop-light.png'), animations: 'disabled' })
  await page.reload()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Consider the mobile experience too')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('finish now')
  await page.getByRole('button', { name: 'Send now' }).click()
  await expect.poll(() => workspace.service.chats.detail(workspace.service.store.list('chats')[0].id).messages.filter(message => message.status !== 'delivered').length).toBe(0)
  await expectChatReady(page)
  await expect(page.locator('.activity-message').filter({ hasText: 'Add a regression test for keyboard navigation' }).last()).toBeVisible()
  await page.goto(workspacePath('/agents'))
  await page.locator('article').filter({ has: page.getByRole('heading', { name: 'Release engineer', exact: true }) }).getByRole('link', { name: 'Start chat' }).click()
  await expect(page.getByRole('group', { name: 'Agent', exact: true }).getByRole('radio', { name: 'Release engineer', exact: true })).toBeChecked()
})
