import { connectClaude } from './accounts'
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

test('a saved quota failure really resumes its session from the inline card', async ({ page, workspace }) => {
  test.setTimeout(120000)
  expect((await connectClaude(workspace, 'Resume quota test', 'fixture-code:resume-quota@example.test')).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Inspect the saved workspace', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  const before = (await workspace.api(`/api/chats/${chat.id}`)).run
  const message = 'You\'ve hit your individual spend limit · your weekly limit resets 3pm (UTC)'
  // Reproduce a pre-fix persisted failure with a real prepared session and workspace.
  workspace.service.store.updateRun(before.id, { status: 'failed', error: message, resumeAvailable: true })
  workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, paused: true })
  workspace.service.store.event(before.id, 'turn.failed', JSON.stringify({ type: 'turn.failed', error: { message } }))
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
  test.setTimeout(120000)
  expect((await connectClaude(workspace)).state).toBe('complete')
  await page.goto(workspacePath('/agents'))
  await authenticateWorkspace(page)
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Claude engineer')
  await grantFullAccess(page)
  await openAgentSection(page, 'Coding agent & model')
  await page.getByRole('button', { name: /^Agent, model and reasoning/ }).click()
  const agentMenu = page.getByRole('dialog', { name: 'Agent & model' })
  await agentMenu.getByRole('radio', { name: 'Claude Code', exact: true }).click()
  await agentMenu.getByRole('radio', { name: 'Sonnet', exact: true }).click()
  await agentMenu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(page.getByRole('button', { name: /^Agent, model and reasoning: Claude Code · Sonnet/ })).toBeVisible()
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
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'fixture:question' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).questions?.filter((q: { status: string }) => q.status === 'pending').length, { timeout: 20000 }).toBe(1)
  const question = (await workspace.api(`/api/chats/${chat.id}`)).questions.find((q: { status: string }) => q.status === 'pending')
  await workspace.api(`/api/chats/${chat.id}/questions/${question.id}/answer`, 'POST', { id: crypto.randomUUID(), answers: { 0: ['Small change'] } })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 20000 }).toBe('succeeded')
  const task = await workspace.api('/api/tasks', 'POST', { name: 'Claude scheduled work', prompt: 'Review the workspace', agentId: agent.id })
  const run = await workspace.api(`/api/tasks/${task.id}/run`, 'POST')
  await expect.poll(async () => (await workspace.api(`/api/runs/${run.id}`)).status, { timeout: 20000 }).toBe('succeeded')
  // The run is recorded with the Claude account it used.
  const account = (await workspace.api('/api/accounts')).accounts.find((a: { provider: string }) => a.provider === 'claude')
  expect((await workspace.api(`/api/runs/${run.id}`)).accountId).toBe(account.id)
})

test('switching coding agents preserves one chat, context and provider selection after reload', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
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
  const picker = page.getByRole('button', { name: /^Agent, model and reasoning/ })
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
  await expect(picker).toHaveAccessibleName('Agent, model and reasoning: Claude Code · Opus Fixture (1M context) · Medium reasoning')
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
  await expect(picker).toHaveAccessibleName(/^Agent, model and reasoning: Claude Code/)
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
  const picker = page.getByRole('button', { name: /^Agent, model and reasoning/ })
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
    await expect(picker).toHaveAccessibleName(`Agent, model and reasoning: Claude Code · ${name}`)
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

test('a plan turn proposes without changes and Implement continues the same chat', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  if (!(await workspace.api('/api/accounts')).accounts.some((a: { provider: string }) => a.provider === 'claude'))
    expect((await connectClaude(workspace)).state).toBe('complete')
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Inspect this workspace', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  const conversation = (await workspace.api(`/api/chats/${chat.id}`)).run.id
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)

  const toggle = page.getByRole('button', { name: 'Plan first' })
  await toggle.click()
  await expect(toggle).toHaveAttribute('aria-pressed', 'true')
  await page.getByRole('textbox', { name: 'Message' }).fill('fixture:plan-turn Plan the composer change')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  const proposal = page.getByTestId('proposed-plan')
  await expect(proposal).toContainText('Change main.rs', { timeout: 30000 })
  await expect(page.locator('.user-bubble').filter({ hasText: 'fixture:plan-turn' }).getByText('Plan', { exact: true })).toBeVisible()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.chatExecution.plan).toBe(true)
  await page.screenshot({ path: testInfo.outputPath('plan-turn-web.png'), animations: 'disabled' })

  // Leaving plan mode is an ordinary message in the same chat, with the agent's usual access.
  await proposal.getByRole('button', { name: 'Implement' }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.chatExecution?.text, { timeout: 30000 }).toBe('Implement the plan you proposed.')
  const implementation = (await workspace.api(`/api/chats/${chat.id}`)).run
  expect(implementation.id).toBe(conversation)
  expect(implementation.chatExecution.plan).toBeUndefined()
  expect(implementation.chatExecution.afterPlan).toBe(true)
  await expect(toggle).toHaveAttribute('aria-pressed', 'false')
  await expect(proposal.getByRole('button', { name: 'Implement' })).toHaveCount(0)
})
