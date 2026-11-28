import type { Page } from '@playwright/test'
import type { Workspace } from './fixtures'
import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectChatReady,
  headerStatus,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('a durable scheduled wait survives restart and resumes without a user message', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Check CI and finish the original task' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const wakeup = {
    id: randomUUID(),
    messageId: detail.run.chatExecution.messageId,
    seconds: 420,
    reason: 'Wait for CI',
    createdAt: Date.now(),
    wakeAt: Date.now() + 420000,
  }
  workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, wakeup })
  workspace.service.store.event(detail.run.id, 'chat.sleep', wakeup.reason, wakeup)
  await workspace.restart()
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  // One card in the conversation tells what it waits for, why and how long.
  const card = page.locator('.chat-wait')
  await expect(card).toHaveCount(1)
  await expect(card).toContainText(/Resumes at .+Wait for CI.*in \d+ min/)
  await expect(headerStatus(page, /^Resumes in \d+ min$/)).toBeVisible()
  await expect(page.getByRole('button', { name: 'Cancel scheduled wait', exact: true })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveAttribute('placeholder', 'Reply now — this cancels the wait')
  // The Fil lists it apart, until it resumes.
  await page.getByRole('tab', { name: /^En cours/ }).click()
  const waiting = page.getByRole('region', { name: 'En cours', exact: true })
  await expect(waiting.locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText(/Resumes at .+\d min/)
  await expect(page.locator('.agent-waiting')).toHaveCount(0)
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await expect(card).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
  }

  await page.getByRole('button', { name: 'Pause wait', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(true)
  // Paused, it still names what it waits for and why.
  await expect(card).toContainText(/Resumes at .+Wait for CI.*Paused/)
  workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, wakeup: { ...wakeup, wakeAt: Date.now() } })
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.status).toBe('succeeded')
  await page.getByRole('button', { name: 'Resume', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.summary).toContain('Cairn resumed this conversation')
  await expect(card).toContainText(/Scheduled time reached.*Wait for CI/)
  await expect(card.getByRole('button', { name: 'Pause wait' })).toHaveCount(0)
  await expect(waiting).toHaveCount(0)
  const resumed = await workspace.api(`/api/chats/${chat.id}`)
  expect(resumed.run.id).toBe(detail.run.id)
  expect(resumed.run.sessionId).toBe(detail.run.sessionId)
  const userEvents = workspace.service.store.events(detail.run.id, 0, 100).filter(event => event.type === 'chat.user')
  expect(userEvents).toHaveLength(1)
  expect(userEvents[0].text).toBe('Check CI and finish the original task')
  expect(resumed.wakeup).toBeNull()
})

for (const lateStart of [false, true]) {
  test(`a scheduled wait card follows its turn and clamps a long reason (late start: ${lateStart})`, async ({ page, workspace }) => {
    const chat = await workspace.api('/api/chats', 'POST', {})
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Ship the batch and check CI' })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    const reason = Array.from({ length: 12 }).fill('Check Quality and Android on the PR, fix failures, then continue the next batch.').join(' ')
    const wakeup = {
      id: randomUUID(),
      messageId: detail.run.chatExecution.messageId,
      seconds: 420,
      reason,
      createdAt: Date.now(),
      wakeAt: Date.now() + 420000,
    }
    workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, wakeup })
    workspace.service.store.event(detail.run.id, 'chat.sleep', reason, wakeup)
    if (lateStart)
      workspace.service.store.event(detail.run.id, 'turn.started', '', {})
    workspace.service.store.event(detail.run.id, 'item.completed', '', { item: { id: 'after-wait', type: 'agent_message', text: 'CI is running; I will resume to check it.' } })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const card = page.locator('.chat-wait')
    const reply = page.getByText('CI is running; I will resume to check it.')
    await expect(card).toHaveCount(1)
    await expect(reply).toBeVisible()
    // The wait starts once the turn ends: its card follows what the agent said after the call.
    expect((await card.boundingBox())!.y).toBeGreaterThan((await reply.boundingBox())!.y)
    const text = card.getByText(reason)
    const clamped = (await text.boundingBox())!.height
    await card.getByRole('button', { name: 'Show more', exact: true }).click()
    expect((await text.boundingBox())!.height).toBeGreaterThan(clamped * 2)
    await card.getByRole('button', { name: 'Show less', exact: true }).click()
    expect((await text.boundingBox())!.height).toBe(clamped)
  })
}

test('stopping a scheduled wait cancels its wakeup and pauses the conversation', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Wait for a remote review' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  workspace.service.store.put('chats', {
    ...workspace.service.store.get('chats', chat.id)!,
    wakeup: {
      id: randomUUID(),
      messageId: detail.run.chatExecution.messageId,
      seconds: 420,
      reason: 'Wait for review',
      createdAt: Date.now(),
      wakeAt: Date.now() + 420000,
    },
  })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  // Its scheduling is outside the loaded history: the pending wait still has its card.
  const card = page.locator('.chat-wait')
  await expect(card).toContainText(/Resumes at .+Wait for review/)
  await page.getByRole('button', { name: 'Cancel scheduled wait', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(true)
  expect((await workspace.api(`/api/chats/${chat.id}`)).wakeup).toBeNull()
  await expect(card).toHaveCount(0)
})

test('a durable condition wait follows a dependency across restart and queue pause', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const agent = workspace.service.store.get('agents', '00000000-0000-4000-8000-000000000001')!
  workspace.service.store.put('agents', { ...agent, access: { ...agent.access, installation: true } })
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Finish after the dependency' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  workspace.service.store.set('deployment-lease', 'condition-wait-fixture', Date.now() + 120000)
  try {
    const dependency = await workspace.api('/api/chats', 'POST', { agentId: agent.id })
    const target = workspace.service.chatRun(dependency.id, 'Condition dependency')
    workspace.service.store.updateRun(target.id, { status: 'queued', finishedAt: null })
    const wakeup = {
      id: randomUUID(),
      messageId: detail.run.chatExecution.messageId,
      reason: 'Join dependency',
      seconds: 420,
      createdAt: Date.now(),
      wakeAt: Date.now() + 420000,
      condition: { type: 'run_finished' as const, runId: target.id },
    }
    // A long generated title and a nearly full window crowd the row.
    workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, wakeup, title: 'Wait for the dependency run to finish before reviewing the release candidate notes' })
    workspace.service.store.updateRun(detail.run.id, {
      contextUsage: {
        contextTokens: 238000,
        contextWindow: 258400,
        cachedTokens: null,
        estimated: false,
      },
    } as any)
    await workspace.restart()
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    await expect(page.locator('.chat-wait')).toContainText(/Waiting for another run to finish.*Join dependency.*up to \d+ min/)
    await expect(headerStatus(page, /^Awaiting result · up to \d+ min$/)).toBeVisible()
    // A countdown stays out of the live region, which would otherwise repeat it every minute.
    await expect(page.locator('.chat-header .sr-only[role="status"]')).toHaveText('')
    // The title gives way before the pills, which never cover the header's actions.
    for (const width of [1440, 1000, 901, 700]) {
      await page.setViewportSize({ width, height: 800 })
      const actions = (await page.locator('.chat-header > div').last().boundingBox())!
      for (const pill of [headerStatus(page, /^Awaiting result/), page.getByTestId('context-indicator')]) {
        const box = (await pill.boundingBox())!
        expect(box.x + box.width).toBeLessThanOrEqual(actions.x)
        if (width >= 1000)
          expect(await pill.locator('.truncate').evaluate(label => label.scrollWidth <= label.clientWidth)).toBe(true)
      }

      // The title gives way but stays readable.
      if (width >= 1000)
        expect((await page.locator('.chat-header h1:not(.sr-only)').boundingBox())!.width).toBeGreaterThan(60)
    }

    await page.setViewportSize({ width: 1280, height: 720 })
    await page.getByRole('tab', { name: /^En cours/ }).click()
    await expect(page.getByRole('region', { name: 'En cours', exact: true }).locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText('Another run')
    await expect(page.locator('.agent-waiting')).toHaveCount(0)
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    }

    await page.getByRole('button', { name: 'Pause wait', exact: true }).click()
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).paused).toBe(true)
    workspace.service.store.delete('deployment-lease')
    await expect.poll(async () => (await workspace.api(`/api/runs/${target.id}`)).status, { timeout: 15000 }).toBe('succeeded')
    expect((await workspace.api(`/api/chats/${chat.id}`)).run.status).toBe('succeeded')
    await page.getByRole('button', { name: 'Resume', exact: true }).click()
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.summary).toContain('Cairn resumed this conversation')
    const resumed = await workspace.api(`/api/chats/${chat.id}`)
    expect(resumed.run.sessionId).toBe(detail.run.sessionId)
    expect(resumed.wakeup).toBeNull()
    expect(workspace.service.store.events(detail.run.id, 0, 100).filter(event => event.type === 'chat.user')).toHaveLength(1)
  }
  finally {
    workspace.service.store.delete('deployment-lease')
  }
})

test('a large grouped wait pages through every condition and resumes once all are in', async ({ page, workspace }) => {
  test.setTimeout(90000)
  const agent = workspace.service.store.get('agents', '00000000-0000-4000-8000-000000000001')!
  workspace.service.store.put('agents', { ...agent, access: { ...agent.access, installation: true } })
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Finish after all dependencies' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  workspace.service.store.set('deployment-lease', 'grouped-wait-fixture', Date.now() + 120000)
  try {
    const targets = await Promise.all(Array.from({ length: 23 }, (_, index) => `Grouped dependency ${index + 1}`).map(async (name) => {
      const dependency = await workspace.api('/api/chats', 'POST', { agentId: agent.id })
      const target = workspace.service.chatRun(dependency.id, name)
      workspace.service.store.updateRun(target.id, { status: 'queued', finishedAt: null })
      return target
    }))
    const first = targets[0]
    const last = targets.at(-1)!
    workspace.service.store.updateRun(first.id, { status: 'failed', finishedAt: Date.now() })
    const conditions = targets.map(target => ({ type: 'run_finished' as const, runId: target.id }))
    const wakeup = {
      id: randomUUID(),
      messageId: detail.run.chatExecution.messageId,
      reason: 'Join all dependencies',
      seconds: 420,
      createdAt: Date.now(),
      wakeAt: Date.now() + 420000,
      condition: { type: 'group' as const, mode: 'all' as const, conditions },
      members: targets.map(() => ({})),
    }
    workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, wakeup })
    workspace.service.store.event(detail.run.id, 'chat.wait', wakeup.reason, wakeup)
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const card = page.locator('.chat-wait')
    await expect(card).toContainText('Waiting for 23 results')
    // The failure already counts as a result: Cairn records it while the other run is pending.
    await expect(card.getByTestId('wait-member')).toHaveCount(10)
    await expect(card.getByTestId('wait-member').first()).toContainText('The run failed')
    await expect(card.getByTestId('wait-member').last()).toContainText('Waiting for it to finish')
    await expect(card).toContainText('1/23')
    await page.getByRole('tab', { name: /^En cours/ }).click()
    await expect(page.getByRole('region', { name: 'En cours', exact: true }).locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText('1 of 23 results')
    await expect(card.getByRole('link', { name: `Open run ${first.id.slice(0, 8)}` })).toHaveAttribute('href', new RegExp(`/runs/${first.id}$`))
    await expect(card.getByRole('button', { name: 'Previous conditions' })).toBeDisabled()
    await card.getByRole('button', { name: 'Next conditions' }).click()
    await expect(card).toContainText('11–20 of 23')
    await card.getByRole('button', { name: 'Next conditions' }).click()
    await expect(card.getByTestId('wait-member')).toHaveCount(3)
    await expect(card).toContainText('21–23 of 23')
    await expect(card.getByRole('button', { name: 'Next conditions' })).toBeDisabled()
    await expect(card.getByRole('link', { name: `Open run ${last.id.slice(0, 8)}` })).toHaveAttribute('href', new RegExp(`/runs/${last.id}$`))
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    }

    for (const target of targets.slice(1))
      workspace.service.store.updateRun(target.id, { status: 'succeeded', finishedAt: Date.now() })
    workspace.service.store.delete('deployment-lease')
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.summary, { timeout: 30000 }).toContain('Cairn resumed this conversation')
    await expect(card).toContainText('All 23 results in')
    await expect(card).toContainText('1 failed · 22 succeeded')
  }
  finally {
    workspace.service.store.delete('deployment-lease')
  }
})

test('verbose tool calls keep their action steps with abbreviated details', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:verbose-tools',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByText('Ready for the next step.', { exact: false }).last()).toBeVisible()
  const actions = page.getByTestId('agent-actions').filter({ hasText: /ran \d+ commands?/i })
  // The fixture also searches once: all 61 actions remain available in bounded groups,
  // the earlier ones arriving with the older pages loaded once the conversation shows.
  await expect(actions.first()).toBeVisible()
  await expect.poll(() => actions.count()).toBeGreaterThanOrEqual(3)
  const count = await actions.count()
  for (let index = 0; index < count; index++) {
    const group = actions.nth(index)
    await group.locator(':scope > button').click()
    const size = await group.getByTestId('agent-step').count()
    expect(size).toBeGreaterThan(0)
    expect(size).toBeLessThanOrEqual(30)
  }

  await expect(page.getByTestId('agent-step')).toHaveCount(61)

  const steps = page.getByTestId('agent-step').filter({ hasText: 'fixture verbose output' })
  await expect(steps).toHaveCount(60)
  await steps.last().click()
  const sheet = page.getByTestId('agent-step-sheet')
  await expect(sheet).toContainText('Completed')
  await expect(sheet).toContainText('Details shortened: arguments and output were omitted.')
  await expect(sheet).not.toContainText('x'.repeat(100))
  for (const width of [1280, 390]) {
    await page.setViewportSize({ width, height: 844 })
    await expect(sheet).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    await page.screenshot({ path: test.info().outputPath(`abbreviated-tool-details-${width}.png`) })
  }
})

test('a delegated team row opens its child conversation directly', async ({ page, workspace }) => {
  const parent = await workspace.api('/api/chats', 'POST', {})
  const child = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${parent.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'Review the send queue.',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${parent.id}`)).run?.status, { timeout: 15000 }).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${parent.id}`)
  workspace.service.store.event(detail.run.id, 'chat.delegated', '', {
    chatId: child.id,
    title: 'Review on another coding agent',
    agentId: child.agentId,
    agentName: 'Reviewer',
    provider: 'codex',
    model: 'gpt-fixture-mini',
    task: 'Review the pushed send queue changes.',
  })
  await page.goto(workspacePath(`/chats/${parent.id}`))
  await authenticateWorkspace(page)
  const team = page.getByTestId('subagent-team')
  await expect(team).toContainText('1 delegated task')
  const header = team.locator(':scope > button')
  if (await header.getAttribute('aria-expanded') === 'false')
    await header.click()
  await team.getByRole('link', { name: /^Review on another coding agent,/ }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${child.id}`))
  await expectChatReady(page)
})

test('subagents show as a team with their own steps and report', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:subagents',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByText('Ready for the next step.', { exact: false }).last()).toBeVisible()
  const team = page.getByTestId('subagent-team')
  await expect(team).toContainText('1 subagent')
  await expect(team).toContainText('1 done')
  // A team that finished without failures folds into its summary.
  const header = team.locator(':scope > button')
  await expect(header).toHaveAttribute('aria-expanded', 'false')
  await header.click()
  const row = team.getByTestId('subagent').filter({ hasText: 'Review the keyboard handling' })
  await expect(row).toContainText('Keyboard handling looks right.')
  await expect(row).toContainText('Done')
  await row.locator(':scope > button').click()
  const detail = row.getByTestId('subagent-detail')
  await expect(detail).toContainText('Check every shortcut.')
  await expect(detail).toContainText('Report')
  await expect(detail).toContainText('gpt-fixture-mini')
  const steps = detail.getByTestId('agent-actions')
  await expect(steps).toContainText('Searched once')
  await steps.locator(':scope > button').click()
  await expect(steps.getByTestId('agent-step').filter({ hasText: 'rg -n shortcut src' })).toHaveCount(1)
  // The subagent's messages stay with it instead of reading as the agent's reply.
  await expect(page.locator('.activity-message').filter({ hasText: 'Keyboard handling looks right.' })).toHaveCount(0)
  // Its full activity opens beside the conversation, with each step's output.
  await row.getByTestId('open-subagent').click()
  const panel = page.getByRole('complementary', { name: 'Subagent activity' }).getByTestId('subagent-panel')
  await expect(panel).toContainText('Review the keyboard handling')
  await expect(panel).toContainText('Done')
  await expect(panel.getByTestId('subagent-steps')).toContainText('rg -n shortcut src')
  await expect(panel).toContainText('Keyboard handling looks right.')
  await page.screenshot({ path: test.info().outputPath('subagent-panel-1280.png'), animations: 'disabled' })
  await panel.getByRole('button', { name: 'Close subagent activity' }).click()
  await expect(panel).toHaveCount(0)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      await team.scrollIntoViewIfNeeded()
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
      await page.screenshot({ path: test.info().outputPath(`subagent-team-${colorScheme}-${width}.png`), animations: 'disabled' })
    }
  }
})

async function waitForWorkingSubagents(workspace: Workspace, chatId: string) {
  await workspace.api(`/api/chats/${chatId}/messages`, 'POST', {
    id: randomUUID(),
    text: 'fixture:subagents-working-team',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chatId}`)).run?.status, { timeout: 15000 }).toBe('running')
  const run = (await workspace.api(`/api/chats/${chatId}`)).run
  const spawned = () => workspace.service.store.events(run.id, 0, 100)
    .filter(event => event.type === 'item.completed' && (event.payload?.item as Record<string, unknown> | undefined)?.type === 'subagent')
    .map(event => event.payload!.item as Record<string, unknown>)
  await expect.poll(() => spawned().length).toBe(2)
  // Mirror Claude's explicit background wait after its subagents have been launched.
  for (const agent of spawned())
    workspace.service.store.event(run.id, 'item.updated', '', { type: 'item.updated', item: { ...agent, task_id: agent.id } })
  workspace.service.store.event(run.id, 'turn.waiting', '', { type: 'turn.waiting', tasks: spawned().map(agent => ({ id: agent.id, description: agent.name })) })
}

// Holds the next queued submission so its row appears while a press is still down. The hold
// has its own bound: a test timeout skips the caller's finally.
async function holdNextSubmission(page: Page, chatId: string, submission: PromiseWithResolvers<void>) {
  await page.route(`**/api/chats/${chatId}/messages`, async (route) => {
    await Promise.race([submission.promise, new Promise(resolve => setTimeout(resolve, 20000))])
    await route.continue()
  })
}

test('working subagents stay in sight beside the composer and open as a drawer on phones', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const submission = Promise.withResolvers<void>()
  try {
    await waitForWorkingSubagents(workspace, chat.id)
    await page.setViewportSize({ width: 1440, height: 900 })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const bar = page.getByTestId('subagent-bar')
    await expect(bar).toContainText('2 running')
    // The bar replaces the working indicator while the agent only waits for its subagent.
    await expect(page.locator('.agent-working')).toHaveCount(0)
    await expect(page.getByText('Waiting for the worker…')).toHaveCount(0)
    const chip = bar.getByTestId('subagent-chip').filter({ hasText: 'Review the keyboard handling' })
    await chip.click()
    await expect(chip).toHaveAttribute('aria-pressed', 'true')
    const panel = page.getByRole('complementary', { name: 'Subagent activity' }).getByTestId('subagent-panel')
    await expect(panel).toContainText('Running')
    await expect(panel.getByTestId('subagent-steps')).toContainText('rg -n shortcut src')
    const firstTab = panel.getByRole('tab', { name: 'Review the keyboard handling' })
    const secondTab = panel.getByRole('tab', { name: 'Check the panel accessibility' })
    await firstTab.focus()
    await firstTab.press('ArrowRight')
    await expect(secondTab).toBeFocused()
    await expect(secondTab).toHaveAttribute('aria-selected', 'true')
    await expect(firstTab).toHaveAttribute('tabindex', '-1')
    await expect(panel.getByRole('tabpanel')).toHaveAccessibleName('Check the panel accessibility')
    await secondTab.press('Home')
    await expect(firstTab).toBeFocused()
    await expect(firstTab).toHaveAttribute('aria-selected', 'true')
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeInViewport()
    await panel.getByRole('button', { name: 'Close subagent activity' }).click()
    await expect(chip).toBeFocused()
    // A click just inside a chip's left edge opens it. WebKit truncates mouse coordinates, so with
    // the edge on a half pixel the click reads as a quarter pixel left of the chip. The first
    // chip's edge only follows the static label: later chips shift as running timers tick.
    await expect(bar.getByTestId('subagent-chip').first()).toContainText('Review the keyboard handling')
    await chip.evaluate((element) => {
      const left = element.getBoundingClientRect().left
      element.style.marginLeft = `${(1.5 - (left % 1)) % 1}px`
    })
    const chipBounds = (await chip.boundingBox())!
    expect(chipBounds.x % 1).toBeCloseTo(0.5)
    await page.mouse.click(chipBounds.x + 0.25, chipBounds.y + chipBounds.height / 2)
    await expect(chip).toHaveAttribute('aria-pressed', 'true')
    await chip.evaluate(element => element.style.removeProperty('margin-left'))
    await page.screenshot({ path: test.info().outputPath('subagent-bar-1440.png'), animations: 'disabled' })

    await page.setViewportSize({ width: 390, height: 844 })
    await expect(page.getByRole('complementary', { name: 'Subagent activity' })).toHaveCount(0)
    const drawer = page.getByRole('dialog', { name: 'Subagent activity' })
    await expect(drawer.getByTestId('subagent-panel')).toContainText('Review the keyboard handling')
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    await page.screenshot({ path: test.info().outputPath('subagent-drawer-390.png'), animations: 'disabled' })
    await drawer.getByRole('button', { name: 'Close subagent activity' }).click()
    await expect(drawer).toHaveCount(0)
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeFocused()
    const followUp = 'Keep going while I check the panel.'
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill(followUp)
    await holdNextSubmission(page, chat.id, submission)
    await page.getByRole('button', { name: 'Send after this turn', exact: true }).click()
    await expect(bar).toContainText('2 running')
    const phoneButton = bar.getByRole('button', { name: /2 subagents working/ })
    await expect(phoneButton).toBeInViewport()
    const buttonBounds = (await phoneButton.boundingBox())!
    const pressX = buttonBounds.x + buttonBounds.width / 2
    const pressY = buttonBounds.y + buttonBounds.height / 2
    await page.mouse.move(pressX, pressY)
    await page.mouse.down()
    submission.resolve()
    try {
      await expect(page.getByTestId('queued-message').getByText(followUp, { exact: true })).toBeVisible()
      // The queue moved the button away from the press before the release.
      await expect.poll(async () => Math.abs((await phoneButton.boundingBox())!.y - buttonBounds.y)).toBeGreaterThan(10)
      await page.mouse.move(pressX + 1, pressY + 1)
    }
    finally {
      await page.mouse.up()
    }

    await expect(drawer).toBeVisible()
    await expect(page.getByTestId('queued-message-sheet')).toHaveCount(0)
    await drawer.getByRole('button', { name: 'Close subagent activity' }).click()
    await expect(drawer).toHaveCount(0)
    await expect(phoneButton).toBeFocused()
    const movedBounds = (await phoneButton.boundingBox())!
    await page.mouse.move(movedBounds.x + movedBounds.width / 2, movedBounds.y + movedBounds.height / 2)
    await page.mouse.down()
    await page.mouse.move(1, 1)
    await page.mouse.up()
    await expect(drawer).toHaveCount(0)

    await phoneButton.press('Enter')
    await expect(drawer).toBeVisible()
  }
  finally {
    submission.resolve()
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).not.toBe('running')
  }
})

test('a touch on working subagents opens them even when the queue moves the button before release', async ({
  page,
  context,
  workspace,
  browserName,
}) => {
  test.skip(browserName !== 'chromium', 'Holding a touch needs Chromium input events')
  test.setTimeout(60000)
  await useRelayForHttpMocks(page)
  const chat = await workspace.api('/api/chats', 'POST', {})
  const submission = Promise.withResolvers<void>()
  try {
    await waitForWorkingSubagents(workspace, chat.id)
    await page.setViewportSize({ width: 390, height: 844 })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const bar = page.getByTestId('subagent-bar')
    await expect(bar).toContainText('2 running')
    const phoneButton = bar.getByRole('button', { name: /2 subagents working/ })
    const drawer = page.getByRole('dialog', { name: 'Subagent activity' })
    const input = await context.newCDPSession(page)

    // A tap that drifts a few pixels past the edge of the unmoved button still opens it.
    const edge = (await phoneButton.boundingBox())!
    const near = { x: edge.x + edge.width / 2, y: edge.y + 2 }
    await input.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [near] })
    await input.send('Input.dispatchTouchEvent', { type: 'touchMove', touchPoints: [{ ...near, y: near.y - 5 }] })
    await input.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await expect(drawer).toBeVisible()
    // The browser's own tap opened it, so the drawer's focus shows no keyboard ring.
    await expect.poll(() => page.evaluate(() => document.activeElement?.matches(':focus-visible'))).toBe(false)
    await drawer.getByRole('button', { name: 'Close subagent activity' }).click()
    await expect(drawer).toHaveCount(0)

    // Content that moves the button after the release, before the browser's tap, takes no tap.
    await page.evaluate(() => window.addEventListener('pointerup', () => {
      const shift = document.createElement('div')
      shift.dataset.testid = 'late-shift'
      shift.style.height = '80px'
      document.querySelector('[data-testid="subagent-bar"]')!.after(shift)
    }, { once: true }))
    const center = { x: edge.x + edge.width / 2, y: edge.y + edge.height / 2 }
    await input.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [center] })
    await input.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await expect(page.getByTestId('late-shift')).toHaveCount(1)
    await expect.poll(async () => Math.abs((await phoneButton.boundingBox())!.y - edge.y)).toBeGreaterThan(10)
    await expect(drawer).toBeVisible()
    await page.evaluate(() => document.querySelector('[data-testid="late-shift"]')?.remove())
    await drawer.getByRole('button', { name: 'Close subagent activity' }).click()
    await expect(drawer).toHaveCount(0)

    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Keep going while I check the panel.')
    await holdNextSubmission(page, chat.id, submission)
    await page.getByRole('button', { name: 'Send after this turn', exact: true }).click()
    const bounds = (await phoneButton.boundingBox())!
    const touch = { x: bounds.x + bounds.width / 2, y: bounds.y + bounds.height / 2 }
    await input.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [touch] })
    submission.resolve()
    try {
      await expect(page.getByTestId('queued-message')).toBeVisible()
      await expect.poll(async () => Math.abs((await phoneButton.boundingBox())!.y - bounds.y)).toBeGreaterThan(10)
    }
    finally {
      await input.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    }

    // The browser's click after the touch would otherwise open the queued message under it.
    await expect(drawer).toBeVisible()
    await expect(page.getByTestId('queued-message-sheet')).toHaveCount(0)
    await drawer.getByRole('button', { name: 'Close subagent activity' }).click()
    await expect(drawer).toHaveCount(0)
    await expect(phoneButton).toBeFocused()

    // A later tap on the queued message still opens it.
    const queued = (await page.getByTestId('queued-message').getByRole('button').first().boundingBox())!
    const tap = { x: queued.x + queued.width / 2, y: queued.y + queued.height / 2 }
    await input.send('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [tap] })
    await input.send('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] })
    await expect(page.getByTestId('queued-message-sheet')).toBeVisible()
  }
  finally {
    submission.resolve()
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).not.toBe('running')
  }
})

test('the agent plan shows once where it was written, then each change where it happened', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:plan' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const plan = page.getByTestId('agent-plan')
  await expect(plan).toHaveCount(1)
  // A finished plan folds into its summary.
  const header = plan.locator(':scope > button')
  await expect(header).toContainText('Plan complete')
  await expect(header).toContainText('4 steps')
  await expect(header).toHaveAttribute('aria-expanded', 'false')
  await header.click()
  await expect(plan).toContainText('Data model first, then the middleware.')
  await expect(plan.getByTestId('plan-step')).toHaveCount(4)
  await expect(plan.locator('[data-testid="plan-step"][data-state="done"]')).toHaveCount(4)
  const updates = page.getByTestId('plan-update')
  await expect(updates).toHaveCount(2)
  await expect(updates.first()).toContainText('Explore the auth middleware')
  await expect(updates.first()).toContainText('Add the session expiry migration')
  await expect(updates.first()).toContainText('Run the checks')
  await expect(updates.first()).toContainText('1/4')
  await expect(updates.last()).toContainText('4/4')
  await expect(page.getByText('Updated the plan')).toHaveCount(0)
  await expect(page.getByTestId('plan-bar')).toHaveCount(0)
  for (const colorScheme of ['light', 'dark'] as const) {
    await page.emulateMedia({ colorScheme })
    for (const width of [1280, 390]) {
      await page.setViewportSize({ width, height: 844 })
      await plan.scrollIntoViewIfNeeded()
      await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
      await page.screenshot({ path: test.info().outputPath(`agent-plan-${colorScheme}-${width}.png`), animations: 'disabled' })
    }
  }
})

test('the plan of the current request stays beside the composer while the agent follows it', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  try {
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:plan-working' })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('running')
    await page.setViewportSize({ width: 1280, height: 844 })
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    const bar = page.getByTestId('plan-bar')
    await expect(bar).toContainText('Add the session expiry migration')
    await expect(bar).toContainText('1/4')
    await expect(bar).toHaveAccessibleName(/^Plan, 1 of 4 done\. Add the session expiry migration\. Show the plan$/)
    await expect(page.getByTestId('agent-plan')).toContainText('Live')
    await bar.click()
    const popover = page.getByTestId('plan-popover')
    await expect(popover).toContainText('1 of 4 done')
    await expect(popover).toContainText('Data model first, then the middleware.')
    await expect(popover.locator('[data-testid="plan-step"][data-state="active"]')).toContainText('Add the session expiry migration')
    await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeInViewport()
    await page.screenshot({ path: test.info().outputPath('plan-bar-1280.png'), animations: 'disabled' })
    await page.keyboard.press('Escape')
    await expect(popover).toHaveCount(0)
    await expect(bar).toBeFocused()

    await page.setViewportSize({ width: 390, height: 844 })
    await expect(bar).toContainText('Add the session expiry migration')
    await bar.click()
    await expect(popover).toBeVisible()
    await expect.poll(() => page.evaluate(() => document.documentElement.scrollWidth - innerWidth)).toBeLessThanOrEqual(1)
    await page.screenshot({ path: test.info().outputPath('plan-bar-390.png'), animations: 'disabled' })
    // Pressing elsewhere closes it.
    await page.getByRole('textbox', { name: 'Message', exact: true }).click()
    await expect(popover).toHaveCount(0)
  }
  finally {
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).not.toBe('running')
  }
})

test('early and nested subagents keep one row and their own steps', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:subagents-lifecycle' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('succeeded')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const team = page.getByTestId('subagent-team').first()
  await expect(team).toContainText('2 subagents')
  await team.locator(':scope > button').click()
  const row = team.locator(':scope > ul > li').filter({ has: page.getByRole('button', { name: /^keyboard, Done,/ }) })
  await row.locator(':scope > button').click()
  const nested = row.getByTestId('subagent-team')
  await expect(nested).toContainText('1 done')
  await nested.locator(':scope > button').click()
  await nested.getByTestId('subagent').locator(':scope > button').click()
  const actions = nested.getByTestId('agent-actions')
  await expect(actions).toContainText('Ran 1 command')
  await actions.locator(':scope > button').click()
  await expect(actions.getByTestId('agent-step')).toContainText('pnpm test')
  await expect(team.locator(':scope > ul > li').filter({ hasText: 'Stopped' })).toHaveCount(1)
})
