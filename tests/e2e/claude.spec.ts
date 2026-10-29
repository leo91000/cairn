import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { connectClaude, limits, seedCodexAccount } from './accounts'
import {
  signIn as authenticateWorkspace,
  createAgent,
  expect,
  expectSingleScroll,
  grantFullAccess,
  openAgentSection,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

let existingAccountIds: Set<string>
let existingChatIds: Set<string>
let existingRunIds: Set<string>

test.beforeEach(async ({ workspace }) => {
  // Independent scenarios share durable data, not an HTTP request budget.
  await workspace.restart()

  existingAccountIds = new Set((await workspace.api('/api/accounts')).accounts.map((account: { id: string }) => account.id))
  existingChatIds = new Set(workspace.service.store.list('chats').map(chat => chat.id))
  existingRunIds = new Set(workspace.service.store.runs().map(run => run.id))
})

test.afterEach(async ({ workspace }) => {
  // The application is worker-scoped, but these accounts and requests belong to
  // one scenario. Stop its pending work before releasing its account leases.
  for (const chat of workspace.service.store.list('chats')) {
    if (!existingChatIds.has(chat.id))
      await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
  }

  for (const run of workspace.service.store.runs()) {
    if (!existingRunIds.has(run.id) && ['queued', 'running', 'waiting'].includes(run.status))
      await workspace.api(`/api/runs/${run.id}/cancel`, 'POST')
  }

  await expect.poll(async () => {
    const { accounts } = await workspace.api('/api/accounts')
    return accounts.filter((account: { id: string }) => !existingAccountIds.has(account.id))
      .flatMap((account: { activeRunIds: string[] }) => account.activeRunIds)
  }, { timeout: 15000 }).toEqual([])

  const { accounts } = await workspace.api('/api/accounts')
  for (const account of accounts) {
    if (!existingAccountIds.has(account.id))
      await workspace.api(`/api/accounts/${account.id}`, 'DELETE')
  }
})

test('a Claude spend limit waits automatically and appears once in the real conversation', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  expect((await connectClaude(workspace, 'Quota test', 'fixture-code:quota@example.test')).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Corrige le dialogue de révocation.', provider: 'claude' })
  try {
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason, { timeout: 30000 }).toContain('Usage exhausted.')
    await page.setViewportSize({ width: 390, height: 844 })
    await page.emulateMedia({ colorScheme: 'dark' })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const card = page.getByRole('status', { name: 'Coding agent quota reached' })
    await expect(card).toBeVisible()
    await expect(card).toHaveCount(1)
    await expect(card.getByText('Work resumes automatically when an account is available.')).toBeVisible()
    await expect(page.locator('.chat-notice')).toHaveCount(0)
    await expect(page.locator('.agent-working')).toHaveCount(0)
    const raw = page.getByText(/^You've hit your individual spend limit/)
    await expect(raw).toBeHidden()
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath('quota-web-mobile-dark.png'), animations: 'disabled' })
    await card.getByText('Technical details', { exact: true }).click()
    await expect(raw).toBeVisible()
    await expect(raw).toHaveCount(1)
    await page.screenshot({ path: testInfo.outputPath('quota-web-details.png'), animations: 'disabled' })
    await card.getByText('Technical details', { exact: true }).click()
    await page.emulateMedia({ colorScheme: 'light' })
    await page.screenshot({ path: testInfo.outputPath('quota-web-mobile-light.png'), animations: 'disabled' })
    await page.setViewportSize({ width: 1440, height: 1000 })
    await page.emulateMedia({ colorScheme: 'dark' })
    await page.screenshot({ path: testInfo.outputPath('quota-web-desktop-dark.png'), animations: 'disabled' })
    await workspace.api(`/api/chats/${chat.id}/pause`, 'POST', { paused: true })
    await expect(page.getByText('Paused', { exact: true }).first()).toBeVisible()
    await expect(card.getByText('Work resumes automatically when an account is available.')).toBeVisible()
    await expect(card.getByRole('button', { name: 'Resume conversation', exact: true })).toHaveCount(0)
    await card.getByRole('link', { name: 'Manage accounts' }).click()
    await expect(page).toHaveURL(/\/connections$/)
  }
  finally {
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
  }
})

for (const initial of ['unavailable', 'error'] as const) {
  test(`quota alternatives recover from an initially ${initial} response without reopening the conversation`, async ({ page, workspace }) => {
    test.setTimeout(60000)
    await connectClaude(workspace, `Quota alternatives ${initial}`, `fixture-code:alternatives-${initial}@example.test`)
    await seedCodexAccount(workspace, 'Available alternative', limits(10, 20))
    await workspace.api('/api/accounts/refresh', 'POST')
    const chat = await workspace.api('/api/chats', 'POST', {})
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Preserve my request. fixture:spend-limit', provider: 'claude' })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason, { timeout: 30000 }).toContain('Usage exhausted.')
    await useRelayForHttpMocks(page)
    let attempts = 0
    let alternativesAvailable = false
    let releaseInitialReads: () => void = () => {}
    const initialReads = new Promise<void>((resolve) => {
      releaseInitialReads = resolve
    })
    await page.route(`**/chats/${chat.id}/coding-agent`, async (route) => {
      attempts++
      const offerAlternatives = alternativesAvailable

      // A stream refresh may abort a read. Its replacement needs the same initial response.
      await initialReads

      if (offerAlternatives) {
        await route.continue()
        return
      }

      if (initial === 'error') {
        await route.fulfill({ status: 500, json: { error: 'Alternatives temporarily unavailable.' } })
        return
      }

      const response = await route.fetch()
      await route.fulfill({ response, json: { ...await response.json(), choices: [] } })
    })
    try {
      await page.goto(workspacePath(`/chats/${chat.id}`))
      await authenticateWorkspace(page)
      const card = page.getByRole('status', { name: 'Coding agent quota reached' })
      await expect(card).toBeVisible()
      await expect.poll(() => attempts).toBeGreaterThan(0)

      // Replace an in-flight read while the quota snapshot refreshes.
      const readsBeforeRefresh = attempts
      const detail = await workspace.api(`/api/chats/${chat.id}`)
      workspace.service.store.event(detail.run.id, 'diagnostic', 'Quota alternatives fixture refresh')
      await expect.poll(() => attempts).toBeGreaterThan(readsBeforeRefresh)
      await expect(card.getByText('Checking alternatives…')).toBeVisible()
      releaseInitialReads()

      if (initial === 'error') {
        await expect(card.getByRole('alert')).toHaveText('Alternatives temporarily unavailable.')
      }
      else {
        await expect(card.getByText('Checking alternatives…')).toBeHidden()
        await expect(card.getByRole('button', { name: 'Continue with Codex', exact: true })).toHaveCount(0)
        await expect(card.getByRole('alert')).toHaveCount(0)
      }

      const initialRequests = attempts
      alternativesAvailable = true
      await expect(card.getByRole('button', { name: 'Continue with Codex', exact: true })).toBeVisible({ timeout: 15000 })
      expect(attempts).toBeGreaterThan(initialRequests)
      await expect(card.getByRole('alert')).toHaveCount(0)
    }
    finally {
      releaseInitialReads()
      await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
    }
  })
}

test('a quota switch response does not restart alternatives polling after leaving the conversation', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await connectClaude(workspace, 'Leaving quota switch', 'fixture-code:leaving-switch@example.test')
  await seedCodexAccount(workspace, 'Available while leaving', limits(10, 20))
  await workspace.api('/api/accounts/refresh', 'POST')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Keep this request. fixture:spend-limit', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason, { timeout: 30000 }).toContain('Usage exhausted.')
  await useRelayForHttpMocks(page)
  let releaseSwitch: () => void = () => {}
  const switchResponse = new Promise<void>((resolve) => {
    releaseSwitch = resolve
  })
  let reads = 0
  let switchDelivered = false
  await page.route(`**/chats/${chat.id}/coding-agent`, async (route) => {
    if (route.request().method() === 'POST') {
      await switchResponse
      await route.fulfill({ json: {} })
      switchDelivered = true
      return
    }

    reads++
    await route.continue()
  })
  try {
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const card = page.getByRole('status', { name: 'Coding agent quota reached' })
    await card.getByRole('button', { name: 'Continue with Codex', exact: true }).click()
    await expect(card.getByText('Switching to Codex…', { exact: true })).toBeVisible()
    await page.getByRole('link', { name: 'Fil', exact: true }).first().click()
    await expect(card).toHaveCount(0)
    const readsAfterLeaving = reads
    releaseSwitch()
    await expect.poll(() => switchDelivered).toBe(true)
    // Observe longer than the five-second polling period, including real HTTP.
    await page.waitForTimeout(5500)
    expect(reads).toBe(readsAfterLeaving)
  }
  finally {
    releaseSwitch()
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
  }
})

test('the quota card continues the pending request with Codex once, preserving files and history', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  await connectClaude(workspace, 'Exhausted for switching', 'fixture-code:switch-running@example.test')
  await seedCodexAccount(workspace, 'Available', limits(10, 20))
  await workspace.api('/api/accounts/refresh', 'POST')
  const chat = await workspace.api('/api/chats', 'POST', {})
  const messageId = crypto.randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: messageId, text: 'Inspect the workspace and preserve my work. fixture:spend-limit', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason, { timeout: 30000 }).toContain('Usage exhausted.')
  const before = (await workspace.api(`/api/chats/${chat.id}`)).run
  const marker = path.join(before.workspace, 'keep-quota-work.txt')
  await writeFile(marker, 'Work done before quota')
  await workspace.api(`/api/chats/${chat.id}/pause`, 'POST', { paused: true })
  const followupId = crypto.randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: followupId, text: 'Keep this follow-up paused', mode: 'queue' })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const card = page.getByRole('status', { name: 'Coding agent quota reached' })
  const continueButton = card.getByRole('button', { name: 'Continue with Codex', exact: true })
  await expect(continueButton).toBeVisible()
  for (const [width, theme] of [[390, 'dark'], [390, 'light'], [1440, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme: theme })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`quota-switch-${width}-${theme}.png`), animations: 'disabled' })
  }

  await continueButton.click()
  await expect.poll(async () => {
    const current = await workspace.api(`/api/chats/${chat.id}`)
    return current.run?.snapshot.agent.provider === 'codex' && current.run.status === 'succeeded'
  }, { timeout: 30000 }).toBe(true)
  const after = await workspace.api(`/api/chats/${chat.id}`)
  expect(after.run.id).toBe(before.id)
  expect(after.run.workspace).toBe(before.workspace)
  expect(after.paused).toBe(true)
  expect(after.messages.find((m: { id: string }) => m.id === followupId)?.status).toBe('queued')
  expect(after.run.chatExecution.messageId).toBe(messageId)
  expect(await readFile(marker, 'utf8')).toBe('Work done before quota')
  expect(workspace.service.store.chatMessages(chat.id).filter((m: { id: string }) => m.id === messageId)).toHaveLength(1)
  await expect(card).toHaveCount(0)
  await expect(page.getByRole('button', { name: /^Agent, model, reasoning and speed: Codex/ })).toBeVisible()
  await page.reload()
  await expect(card).toHaveCount(0)
})

for (const state of ['queued', 'failed'] as const) {
  test(`a ${state} quota switches the same request through the API and rejects stale identities`, async ({ workspace }) => {
    test.setTimeout(120000)
    await connectClaude(workspace, `Initially available ${state}`, `fixture-code:${state}@example.test`)
    const codex = await seedCodexAccount(workspace, 'Available', limits(10, 20))
    await workspace.api('/api/accounts/refresh', 'POST')
    const chat = await workspace.api('/api/chats', 'POST', {})
    if (state === 'queued') {
      const accounts = await workspace.api('/api/accounts')
      for (const claude of accounts.accounts.filter((a: { provider: string }) => a.provider === 'claude'))
        workspace.service.store.put('agentAccounts', { ...workspace.service.store.get('agentAccounts', claude.id), exhausted: { at: Date.now(), model: '', usage: null } })
    }

    const messageId = crypto.randomUUID()
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: messageId, text: 'Preserve this request', provider: 'claude' })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe(state === 'queued' ? 'queued' : 'succeeded')
    const before = (await workspace.api(`/api/chats/${chat.id}`)).run
    if (state === 'queued') {
      await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason).toContain('account with available usage.')
    }
    else {
      workspace.service.store.updateRun(before.id, { status: 'failed', error: 'Account usage is exhausted.' })
      workspace.service.store.event(before.id, 'turn.failed', 'Account usage is exhausted.', { type: 'turn.failed', error: { code: 'usage_limit_reached', message: 'Account usage is exhausted.' } })
    }

    const endpoint = `/api/chats/${chat.id}/coding-agent`
    const options = await workspace.api(endpoint)
    expect(options.choices.map((c: { provider: string }) => c.provider)).toEqual(['codex'])
    const request = {
      runId: options.runId,
      turnId: options.turnId,
      requestId: crypto.randomUUID(),
      provider: 'codex',
    }
    await expect(workspace.api(endpoint, 'POST', { ...request, turnId: 'stale-turn' })).rejects.toThrow()
    await workspace.api(endpoint, 'POST', request)
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.snapshot.agent.provider, { timeout: 30000 }).toBe('codex')
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
    await workspace.api(`/api/accounts/${codex.id}`, 'PATCH', { enabled: false })
    await workspace.api(endpoint, 'POST', request)
    const after = await workspace.api(`/api/chats/${chat.id}`)
    expect(after.run.id).toBe(before.id)
    expect(after.run.chatExecution.messageId).toBe(messageId)
    expect(after.run.snapshot.agent.provider).toBe('codex')
    expect(after.codingAgentSwitch).toBeNull()
  })
}

test('a saved quota failure really resumes its session from the inline card', async ({ page, workspace }) => {
  test.setTimeout(120000)
  expect((await connectClaude(workspace, 'Resume quota test', 'fixture-code:resume-quota@example.test')).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Inspect the saved workspace', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  const before = (await workspace.api(`/api/chats/${chat.id}`)).run
  const message = 'You\'ve hit your individual spend limit · your weekly limit resets 3pm (UTC)'
  // A saved structured failure keeps its real prepared session and workspace.
  workspace.service.store.updateRun(before.id, { status: 'failed', error: message, resumeAvailable: true })
  workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, paused: true })
  workspace.service.store.event(before.id, 'turn.failed', message, { type: 'turn.failed', error: { message } })
  workspace.service.store.event(before.id, 'status', 'failed')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const card = page.getByRole('status', { name: 'Coding agent quota reached' })
  await expect(card).toBeVisible()
  await expect(page.locator('.chat-notice')).toHaveCount(0)
  await card.getByRole('button', { name: 'Resume conversation', exact: true }).click()
  await expect.poll(async () => {
    const current = await workspace.api(`/api/chats/${chat.id}`)
    return current.run?.status === 'succeeded' && current.run.resumeCount > (before.resumeCount ?? 0) && !current.paused
  }, { timeout: 30000 }).toBe(true)
  const after = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(after.sessionId).toBe(before.sessionId)
  expect(after.workspace).toBe(before.workspace)
  await expect(card).toHaveCount(0)
  await expect(page.locator('.chat-notice')).toHaveCount(0)
  await expect(page.getByText(/^You've hit your individual spend limit/)).toHaveCount(0)
})

test('Claude agents, chats, questions and scheduled work run on a connected Claude account', async ({ page, workspace }, testInfo) => {
  // Start this independent questionnaire journey with a fresh request budget.
  await workspace.restart()
  test.setTimeout(120000)
  expect((await connectClaude(workspace)).state).toBe('complete')
  await page.goto(workspacePath('/agents'))
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Claude engineer')
  await grantFullAccess(page)
  await openAgentSection(page, 'Coding agent & model')
  await page.getByRole('button', { name: /^Agent, model, reasoning and speed/ }).click()
  const agentMenu = page.getByRole('dialog', { name: 'Agent & model' })
  await agentMenu.getByRole('radio', { name: 'Claude Code', exact: true }).click()
  await agentMenu.getByRole('radio', { name: 'Sonnet', exact: true }).click()
  await agentMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(page.getByRole('button', { name: /^Agent, model, reasoning and speed: Claude Code · Sonnet/ })).toBeVisible()
  await createAgent(page)
  const agent = (await workspace.api('/api/agents')).find((a: { name: string }) => a.name === 'Claude engineer')
  expect(agent.provider).toBe('claude')
  const chat = await workspace.api('/api/chats', 'POST', { agentId: agent.id })
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Inspect this workspace' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  const first = await workspace.api(`/api/chats/${chat.id}`)
  expect(first.run.summary).toContain('Claude fixture completed')
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Continue reviewing' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.resumeCount, { timeout: 20000 }).toBeGreaterThan(0)
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await expect(page.getByText('Claude fixture completed', { exact: true })).toHaveCount(2)
  await expectSingleScroll(page)
  await page.screenshot({ path: testInfo.outputPath('claude-chat-mobile.png') })
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'fixture:cairn-question' })
  await expect.poll(async () => {
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    if (detail.run?.error)
      throw new Error(detail.run.error)
    return detail.questions?.filter((q: { status: string }) => q.status === 'pending').length
  }, { timeout: 20000 }).toBe(1)
  const question = (await workspace.api(`/api/chats/${chat.id}`)).questions.find((q: { status: string }) => q.status === 'pending')
  await workspace.api(`/api/chats/${chat.id}/questions/${question.id}/answer`, 'POST', { id: crypto.randomUUID(), answers: { approach: { selected: ['small'], comment: 'Keep it focused' } } })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).questions.find((q: { id: string }) => q.id === question.id)?.status).toBe('answered')
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  const task = await workspace.api('/api/tasks', 'POST', { settings: { name: 'Claude scheduled work', prompt: 'Review the workspace', agentId: agent.id } })
  const run = await workspace.api(`/api/tasks/${task.id}/run`, 'POST')
  await expect.poll(async () => (await workspace.api(`/api/runs/${run.id}`)).status, { timeout: 20000 }).toBe('succeeded')
  // The run is recorded with the Claude account it used.
  const completed = await workspace.api(`/api/runs/${run.id}`)
  const account = (await workspace.api('/api/accounts')).accounts.find((a: { id: string }) => a.id === completed.accountId)
  expect(account?.provider).toBe('claude')
})

test('switching coding agents preserves one chat, context and provider selection after reload', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  await seedCodexAccount(workspace, 'Provider switching', limits(10, 20))
  await workspace.api('/api/accounts/refresh', 'POST')
  if (!(await workspace.api('/api/accounts')).accounts.some((a: { provider: string }) => a.provider === 'claude'))
    expect((await connectClaude(workspace)).state).toBe('complete')
  await page.goto(workspacePath('/connections'))
  await authenticateWorkspace(page)
  // Password verification can be slow in an unoptimized native test build.
  await expect(page.getByRole('region', { name: 'Claude Code accounts' })).toBeVisible({ timeout: 30000 })
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Preserve the existing design. Work on this conversation.' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const first = await workspace.api(`/api/chats/${chat.id}`)
  await page.goto(workspacePath(`/chats/${chat.id}`))
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  await picker.click()
  await expect(menu.getByRole('radio', { name: 'Codex', exact: true })).toHaveAttribute('aria-checked', 'true')
  await menu.getByRole('radio', { name: 'Claude Code', exact: true }).click()
  await expect(menu.getByText('The next message starts a new Claude Code session', { exact: false })).toBeVisible()
  // Claude's own "default" alias is the same choice as the provider default row.
  await expect(menu.getByRole('radio', { name: 'Default (recommended)', exact: true })).toHaveCount(0)
  await expect(menu.getByRole('radio', { name: 'Claude Code default · Opus Fixture (1M context)', exact: true })).toHaveAccessibleDescription('Opus with 1M context · Best for everyday tasks')
  await expect(menu.getByRole('radio', { name: 'Claude Code default · Opus Fixture (1M context)', exact: true })).toHaveAttribute('aria-checked', 'true')
  // The picker value names the model and effort behind the default, not the alias.
  await expect(picker).toHaveAccessibleName('Agent, model, reasoning and speed: Claude Code · Opus Fixture (1M context) · Medium reasoning')
  await menu.getByRole('radio', { name: 'Sonnet', exact: true }).click()
  await expect(menu.getByRole('radiogroup', { name: 'Reasoning' })).toBeVisible()
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(menu).toHaveCount(0)
  for (const width of [1440, 390, 320]) {
    await page.setViewportSize({ width, height: width === 320 ? 600 : 900 })
    await expectSingleScroll(page)
    await expect(page.getByRole('button', { name: 'Send', exact: true })).toBeInViewport()
    await expect(picker).toBeInViewport()
    await page.screenshot({ path: testInfo.outputPath(`chat-provider-${width}.png`), animations: 'disabled' })
    await picker.click()
    await expect(menu.getByRole('radio', { name: 'Sonnet', exact: true })).toBeInViewport()
    const bounds = await menu.boundingBox()
    expect(bounds!.x).toBeGreaterThanOrEqual(0)
    expect(bounds!.x + bounds!.width).toBeLessThanOrEqual(width + 1)
    await page.screenshot({ path: testInfo.outputPath(`chat-provider-menu-${width}.png`), animations: 'disabled' })
    await page.keyboard.press('Escape')
    await expect(menu).toHaveCount(0)
  }

  await page.getByLabel('Message', { exact: true }).fill('Continue with Claude using the previous decisions.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => {
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    return detail.run?.snapshot.agent.provider === 'claude' && detail.run.status === 'succeeded'
  }).toBe(true)
  const second = await workspace.api(`/api/chats/${chat.id}`)
  expect(second.runId).toBe(first.runId)
  expect(second.run.workspace).toBe(first.run.workspace)
  expect(second.run.chatExecution.context).toContain('Preserve the existing design')
  await page.reload()
  await expect(picker).toHaveAccessibleName(/^Agent, model, reasoning and speed: Claude Code/)
  await picker.click()
  await expect(menu.getByRole('radio', { name: 'Claude Code', exact: true })).toHaveAttribute('aria-checked', 'true')
  await menu.getByRole('radio', { name: 'Codex', exact: true }).click()
  await page.keyboard.press('Escape')
  await page.getByLabel('Message', { exact: true }).fill('Return to Codex and keep the work.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => {
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    return detail.run?.snapshot.agent.provider === 'codex' && detail.run.status === 'succeeded'
  }).toBe(true)
  const last = await workspace.api(`/api/chats/${chat.id}`)
  expect(last.runId).toBe(first.runId)
  expect(last.run.chatExecution.context).toContain('Claude fixture completed')
  expect(workspace.service.store.chatMessages(chat.id)).toHaveLength(3)
})

test('an agent idle until its background build finishes says it is waiting, not working', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  await useRelayForHttpMocks(page)
  if (!(await workspace.api('/api/accounts')).accounts.some((a: { provider: string }) => a.provider === 'claude'))
    expect((await connectClaude(workspace)).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'fixture:background-slow', provider: 'claude' })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const waiting = page.locator('.agent-waiting')
  await expect(waiting).toBeVisible({ timeout: 20000 })
  await expect(waiting).toHaveAccessibleName('Waiting for a background task: Wait for build. The agent resumes when it finishes.')
  await expect(waiting.getByText('Wait for build', { exact: true })).toBeVisible()
  await expect(page.locator('.agent-working')).toHaveCount(0)
  await expect(page.getByText('Background task', { exact: true }).first()).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('claude-background-wait.png'), animations: 'disabled' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  await expect(waiting).toHaveCount(0)
  await expect(page.getByText('Build checked and task finished', { exact: true })).toBeVisible()
})

test('Claude retries and warnings read as the engine reported them, not as failures', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  await useRelayForHttpMocks(page)
  if (!(await workspace.api('/api/accounts')).accounts.some((a: { provider: string }) => a.provider === 'claude'))
    expect((await connectClaude(workspace)).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'fixture:engine-retry-slow', provider: 'claude' })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const retry = page.locator('.chat-notice').filter({ hasText: 'Claude Code is retrying the request · attempt 1/10' })
  await expect(retry).toBeVisible({ timeout: 20000 })
  await expect(retry.getByText('Servers overloaded · HTTP 529 · next attempt after under a second')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('claude-engine-retry.png'), animations: 'disabled' })

  // Once work continues the retry leaves the conversation; the engine's warning stays.
  await expect(page.getByText('Answered after the retries', { exact: true })).toBeVisible({ timeout: 20000 })
  await expect(retry).toHaveCount(0)
  const warning = page.locator('.chat-notice').filter({ hasText: 'Claude Code warning' })
  await expect(warning).toBeVisible()
  await expect(warning.getByText('Context is almost full.')).toBeVisible()
  await expect(page.getByText('The agent ran into an error')).toHaveCount(0)
  await page.screenshot({ path: testInfo.outputPath('claude-engine-warning.png'), animations: 'disabled' })
})

test('a Claude limit that stops requests waits for an account and shows the reported reset', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  expect((await connectClaude(workspace, 'Structured limit', 'fixture-code:structured-limit@example.test')).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'fixture:usage-limit', provider: 'claude' })
  try {
    // The provider's wording is not a known quota message: only the engine's report says so.
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason, { timeout: 30000 }).toContain('Usage exhausted.')
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const card = page.getByRole('status', { name: 'Coding agent quota reached' })
    await expect(card).toBeVisible()
    const reset = await page.evaluate(() => new Date(1893499200000).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }))
    await expect(card.getByText(/Reset announced:/)).toContainText(reset)
    await expect(page.locator('.chat-notice').filter({ hasText: '5-hour limit reached' })).toHaveCount(0)
    await page.screenshot({ path: testInfo.outputPath('claude-structured-limit.png'), animations: 'disabled' })
  }
  finally {
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
  }
})

test('Claude default names follow available metadata without pinning the model', async ({ page, workspace }, testInfo) => {
  await useRelayForHttpMocks(page)
  let displayName = 'Opus Fixture (1M context)'
  let description = 'Model recommended for your subscription'
  await page.route('**/claude/models', route => route.fulfill({
    json: {
      models: [
        {
          model: 'default',
          displayName,
          description,
          isDefault: true,
          hidden: false,
          defaultReasoningEffort: '',
          supportedReasoningEfforts: [],
        },
        {
          model: 'sonnet',
          displayName: 'Sonnet',
          description: '',
          isDefault: false,
          hidden: false,
          defaultReasoningEffort: '',
          supportedReasoningEfforts: [],
        },
      ],
      checkedAt: Date.now(),
      stale: false,
      error: '',
    },
  }))
  await page.goto(workspacePath('/agents/new'))
  await authenticateWorkspace(page)
  await page.getByLabel('Name', { exact: true }).fill('Claude default labels')
  await openAgentSection(page, 'Coding agent & model')
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await picker.click()
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  await menu.getByRole('radio', { name: 'Claude Code', exact: true }).click()
  for (const [name, details] of [
    ['Opus Fixture (1M context)', 'Model recommended for your subscription'],
    ['Opus description', 'Opus description · Details'],
    ['claude-opus-fixture[1m]', ''],
    ['Default (recommended)', ''],
  ]) {
    displayName = name
    description = details
    await menu.getByRole('button', { name: 'Refresh models', exact: true }).click()
    const title = name.startsWith('Default') ? 'Claude Code default' : `Claude Code default · ${name}`
    const choice = menu.getByRole('radio', { name: title, exact: true })
    await expect(choice).toBeVisible()
    await menu.getByRole('radio', { name: 'Sonnet', exact: true }).click()
    await choice.click()
    await expect(choice).toHaveAttribute('aria-checked', 'true')
    await menu.getByRole('button', { name: 'Close', exact: true }).click()
    await expect(picker).toHaveAccessibleName(`Agent, model, reasoning and speed: Claude Code · ${name}`)
    await picker.click()
  }

  displayName = 'Opus Fixture (1M context)'
  await menu.getByRole('button', { name: 'Refresh models', exact: true }).click()
  await expect(menu.getByRole('radio', { name: 'Claude Code default · Opus Fixture (1M context)', exact: true })).toHaveAttribute('aria-checked', 'true')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: testInfo.outputPath('claude-default-mobile-dark.png'), animations: 'disabled' })
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await createAgent(page)
  const saved = (await workspace.api('/api/agents')).find((agent: { name: string }) => agent.name === 'Claude default labels')
  expect(saved).toMatchObject({ provider: 'claude', model: '', reasoning: '' })
})

test('the quota card switches to OpenCode with an available model and preserves the sandbox', async ({ page, workspace }) => {
  test.setTimeout(120000)
  await connectClaude(workspace, 'OpenCode handoff', 'fixture-code:switch-opencode@example.test')
  await workspace.api('/api/accounts', 'POST', {
    provider: 'opencode',
    name: 'Quota alternative Zen',
    modelProvider: 'opencode',
    apiKey: 'synthetic-quota-zen-key',
  })
  const chat = await workspace.api('/api/chats', 'POST', {})
  const messageId = crypto.randomUUID()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: messageId, text: 'Continue my work. fixture:spend-limit', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason, { timeout: 30000 }).toContain('Usage exhausted.')
  const before = (await workspace.api(`/api/chats/${chat.id}`)).run
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const card = page.getByRole('status', { name: 'Coding agent quota reached' })
  await card.getByRole('button', { name: 'Continue with OpenCode', exact: true }).click()
  await expect.poll(async () => {
    const run = (await workspace.api(`/api/chats/${chat.id}`)).run
    return run?.snapshot.agent.provider === 'opencode' && run.status === 'succeeded'
  }, { timeout: 30000 }).toBe(true)
  const after = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(after.snapshot.agent.model).toMatch(/^auto\/.+/)
  expect(after.snapshot.agent.access).toEqual(before.snapshot.agent.access)
  expect(after.chatExecution.messageId).toBe(messageId)
  expect(after.workspace).toBe(before.workspace)
  await expect(card).toHaveCount(0)
})
