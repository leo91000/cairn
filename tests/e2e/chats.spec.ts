import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectChatReady,
  expectSingleScroll,
  initializeRepository,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

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

test('new conversation on the same route clears an uncertain submission with identical content', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const attempts: { id: string, chatId: string }[] = []
  await page.route(/\/chats\/([^/]+)\/messages$/, async (route) => {
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
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page.getByText('Connection unavailable. Please try again.', { exact: true })).toBeVisible()
  await expect(page).toHaveURL(workspacePath('/chats'))
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('dialog', { name: 'Chat details', exact: true }).getByRole('button', { name: 'New chat', exact: true }).click()
  await expect(composer).toHaveValue('')
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
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('dialog', { name: 'Chat details', exact: true }).getByRole('button', { name: 'New chat', exact: true }).click()
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
  await expect(page.getByText(/^Resumes in \d+ min$/).first()).toBeVisible()
  await expect(page.getByRole('button', { name: 'Cancel scheduled wait', exact: true })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'Message' })).toHaveAttribute('placeholder', 'Reply now — this cancels the wait')
  // The Fil lists it apart, until it resumes.
  const waiting = page.getByRole('region', { name: 'Waiting', exact: true })
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

test('a scheduled wait card follows the turn that scheduled it and clamps a long reason', async ({ page, workspace }) => {
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
    const task = await workspace.api('/api/tasks', 'POST', {
      settings: {
        name: 'Condition dependency',
        prompt: 'Other work',
        agentId: agent.id,
        worktree: false,
      },
    })
    const target = await workspace.api(`/api/tasks/${task.id}/run`, 'POST', {})
    const wakeup = {
      id: randomUUID(),
      messageId: detail.run.chatExecution.messageId,
      reason: 'Join dependency',
      seconds: 420,
      createdAt: Date.now(),
      wakeAt: Date.now() + 420000,
      condition: { type: 'run_finished' as const, runId: target.id },
    }
    workspace.service.store.put('chats', { ...workspace.service.store.get('chats', chat.id)!, wakeup })
    await workspace.restart()
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await authenticateWorkspace(page)
    await expect(page.locator('.chat-wait')).toContainText(/Waiting for another run to finish.*Join dependency.*up to \d+ min/)
    await expect(page.getByText(/^Awaiting result · up to \d+ min$/).first()).toBeVisible()
    await expect(page.getByRole('region', { name: 'Waiting', exact: true }).locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText('Another run')
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
      const task = await workspace.api('/api/tasks', 'POST', {
        settings: {
          name,
          prompt: 'Other work',
          agentId: agent.id,
          worktree: false,
        },
      })
      return workspace.api(`/api/tasks/${task.id}/run`, 'POST', {})
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
    await expect(page.getByRole('region', { name: 'Waiting', exact: true }).locator(`[data-fil-key="chat:${chat.id}"]`)).toContainText('1 of 23 results')
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

test('working subagents stay in sight beside the composer and open as a drawer on phones', async ({ page, workspace }) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  try {
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
      id: randomUUID(),
      text: 'fixture:subagents-working-team',
    })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('running')
    const run = (await workspace.api(`/api/chats/${chat.id}`)).run
    const spawned = () => workspace.service.store.events(run.id, 0, 100)
      .filter(event => event.type === 'item.completed' && (event.payload?.item as Record<string, unknown> | undefined)?.type === 'subagent')
      .map(event => event.payload!.item as Record<string, unknown>)
    await expect.poll(() => spawned().length).toBe(2)
    // Mirror Claude's explicit background wait after its subagents have been launched.
    for (const agent of spawned())
      workspace.service.store.event(run.id, 'item.updated', '', { type: 'item.updated', item: { ...agent, task_id: agent.id } })
    workspace.service.store.event(run.id, 'turn.waiting', '', { type: 'turn.waiting', tasks: spawned().map(agent => ({ id: agent.id, description: agent.name })) })
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
    await chip.click()
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
    await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Keep going while I check the panel.')
    await page.getByRole('button', { name: 'Queue', exact: true }).click()
    await expect(bar).toContainText('2 running')
    await bar.getByRole('button', { name: /2 subagents working/ }).click()
    await expect(page.getByRole('dialog', { name: 'Subagent activity' })).toBeVisible()
  }
  finally {
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

test('conversation switcher preserves drafts and supports search, keyboard and mobile sheets', async ({ page, workspace }) => {
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
    // Desktop lists conversations in the Fil column; tablets and phones open them from the header.
    for (const width of [800, 390, 320]) {
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

  // Crossing a breakpoint while open hides the tablet trigger: focus returns to the ⋯ menu instead.
  await page.setViewportSize({ width: 800, height: 900 })
  await trigger.click()
  await expect(dialog).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 900 })
  expect((await dialog.boundingBox())!.y).toBeGreaterThan(40)
  await page.keyboard.press('Escape')
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
  await panel.getByRole('button', { name: 'Search conversations', exact: true }).click()
  await expect(page.getByRole('searchbox', { name: 'Search conversations' })).toBeFocused()
  await page.keyboard.press('Escape')
  await details.click()
  await panel.getByRole('button', { name: 'Open activity fullscreen', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Fullscreen activity' })).toBeVisible()
  await page.getByRole('button', { name: 'Exit fullscreen', exact: true }).click()
  await expect(details).toBeFocused()
  await details.click()
  await panel.getByRole('button', { name: 'Follow latest output', exact: true }).click()
  await expect(panel).toHaveCount(0)
  await details.click()
  await panel.getByRole('button', { name: 'Search workspace', exact: true }).click()
  await expect(page.getByRole('dialog', { name: 'Search and commands' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(details).toBeFocused()
  await details.click()
  await panel.getByRole('button', { name: /Appearance:/ }).click()
  await panel.locator('.theme-popover').getByText('Dark', { exact: true }).click()
  await expect(panel.getByRole('button', { name: 'Appearance: dark' })).toBeVisible()
  await page.keyboard.press('Escape')
  await details.click()
  await panel.getByRole('button', { name: 'Workspace navigation', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/'))
  await expect(page.getByRole('navigation', { name: 'Quick navigation' })).toBeVisible()
  await page.goBack()
  await expect(message).toHaveValue('Keep my draft while browsing details')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeVisible()
  // Desktop keeps one header row: fullscreen and the same ⋯ menu, no separate activity toolbar.
  await expect(page.locator('.activity-toolbar')).not.toBeVisible()
  await expect(page.getByRole('button', { name: 'Open activity fullscreen', exact: true })).toBeVisible()
  await expect(details).toBeVisible()
  await expect(message).toHaveValue('Keep my draft while browsing details')
  await page.setViewportSize({ width: 390, height: 844 })
  // New chat moves from the phone header into the ⋯ menu.
  await expect(page.getByRole('link', { name: 'New chat', exact: true })).toBeHidden()
  await details.click()
  await panel.getByRole('button', { name: 'New chat', exact: true }).click()
  await expect(page).toHaveURL(workspacePath('/chats'))
  await details.click()
  await expect(panel.getByText('New conversation', { exact: true })).toBeVisible()
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

  // A conversation keeps one row: agent, title and installation, files and the ⋯ menu.
  await page.goto(workspacePath(`/chats/${chat.id}`))
  const header = page.locator('.chat-header')
  await expect(header.getByRole('button', { name: 'Conversations', exact: true })).toContainText('Browser workspace')
  await expect(header.getByRole('button', { name: 'Conversations', exact: true })).toHaveAccessibleDescription('Latest harness: Codex')
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
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await expect(userMessages.filter({ hasText: 'A follow-up for the next reply' })).toHaveCount(0)
  const steer = 'Focus on accessibility. fixture:slow-delivery'
  await composer.fill(steer)
  await page.getByRole('button', { name: 'Steer now', exact: true }).click()
  await expect(userMessages.filter({ hasText: steer }).getByRole('status')).toHaveText('Sending to agent…')
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.screenshot({ path: test.info().outputPath('steer-and-real-queue-mobile.png'), animations: 'disabled' })
  await expect(userMessages.filter({ hasText: steer }).getByRole('status')).toHaveCount(0, { timeout: 15000 })
  await expect(userMessages.filter({ hasText: steer })).toHaveCount(1)
  await page.getByRole('button', { name: 'Stop response', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Resume', exact: true })).toBeVisible()
  await composer.fill('Wait until I resume')
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('conversation-queue').getByRole('status')).toHaveText('Queue paused · nothing is sent · 2')
  await expect(userMessages.filter({ hasText: 'Wait until I resume' })).toHaveCount(0)
  await page.screenshot({ path: test.info().outputPath('paused-queue-mobile.png'), animations: 'disabled' })
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

test('starts project chats, steers, edits the queue and preserves a compact mobile composer', async ({ page, workspace }) => {
  initializeRepository(workspace.projectPath)
  await page.goto(workspacePath('/tasks'))
  await authenticateWorkspace(page)
  await expect(page.locator('.task-focus-detail')).toBeVisible()
  await page.goto(workspacePath('/projects'))
  await page.getByRole('link', { name: 'Start chat' }).first().click()
  await expect(page.getByRole('combobox', { name: 'Chat agent' })).toHaveValue('Main agent')
  await expect(page.getByRole('combobox', { name: 'Chat project' })).toHaveValue('Design system')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('chat-new-desktop-light.png'), animations: 'disabled' })
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Review our component architecture. fixture:chat-hang')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  await expect(page.getByText('Working', { exact: true }).first()).toBeVisible()
  await expect(page.getByTestId('agent-actions').filter({ hasText: 'Followed the run' })).toHaveCount(0)
  await expect(page.locator('.activity-message').filter({ hasText: 'Review our component architecture' })).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Add a regression test for the composer')
  await page.getByRole('button', { name: 'Queue', exact: true }).click()
  await expect(page.getByTestId('queued-message')).toHaveCount(1)
  await page.getByRole('button', { name: /^Queued message options/ }).first().click()
  await page.getByRole('dialog', { name: 'Queued message' }).getByRole('button', { name: 'Edit', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Add a regression test for keyboard navigation')
  await page.getByRole('button', { name: 'Save', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Focus on accessibility first.')
  await page.getByRole('button', { name: 'Steer now' }).click()
  await expect(page.locator('.activity-message').filter({ hasText: 'Focus on accessibility first.' }).first()).toBeVisible()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Consider the mobile experience too')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await expect(page.getByRole('navigation', { name: 'Workspace navigation' })).toBeHidden()
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Queue', exact: true })).toBeInViewport()
  await page.screenshot({ path: test.info().outputPath('chat-mobile-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ path: test.info().outputPath('chat-mobile-light.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 320, height: 600 })
  await expect(page.getByRole('button', { name: 'Queue', exact: true })).toBeInViewport()
  await page.emulateMedia({ colorScheme: 'dark' })
  await expectSingleScroll(page)
  await expect(page.getByRole('button', { name: 'Steer now' })).toBeInViewport()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.screenshot({ path: test.info().outputPath('chat-desktop-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })
  await page.screenshot({ path: test.info().outputPath('chat-desktop-light.png'), animations: 'disabled' })
  await page.reload()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toHaveValue('Consider the mobile experience too')
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('finish now')
  await page.getByRole('button', { name: 'Steer now' }).click()
  await expect.poll(() => workspace.service.chats.detail(workspace.service.store.list('chats')[0].id).messages.filter(message => message.status !== 'delivered').length).toBe(0)
  await expectChatReady(page)
  await expect(page.locator('.activity-message').filter({ hasText: 'Add a regression test for keyboard navigation' }).last()).toBeVisible()
  await page.goto(workspacePath('/agents'))
  await page.locator('article').filter({ has: page.getByRole('heading', { name: 'Release engineer', exact: true }) }).getByRole('link', { name: 'Start chat' }).click()
  await expect(page.getByRole('combobox', { name: 'Chat agent' })).toHaveValue('Release engineer')
})

test('withdraws an obsolete question without sending an answer on desktop and mobile', async ({ page, workspace }) => {
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
  await expectChatReady(page)
  const chatKey = new URL(page.url()).pathname.split('/').at(-1)!
  const chatId = (await workspace.api(`/api/chats/${chatKey}`)).id
  expect(workspace.service.questions.list(chatId)[0].status).toBe('answered')
  await page.setViewportSize({ width: 390, height: 844 })
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('link', { name: 'Question notifications' }).click()
  await expect(page).toHaveURL(workspacePath('/settings/account'))
  await expect(page.getByRole('heading', { name: 'Question notifications' })).toBeVisible()
  await page.screenshot({ path: test.info().outputPath('notifications-mobile.png'), animations: 'disabled' })
})

test('installation push registration is removed in favor of the beacon account', async ({ page, workspace }) => {
  await workspace.restart()
  await page.goto(workspacePath('/chats'))
  await authenticateWorkspace(page)
  // Notifications belong to the account, not to a conversation.
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await page.getByRole('link', { name: 'Question notifications' }).click()
  await expect(page).toHaveURL(workspacePath('/settings/account'))
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

test('conversation deletion is visible, confirmed and permanent on desktop and mobile', async ({ page, workspace }, testInfo) => {
  test.setTimeout(60000)
  const chat = await workspace.api('/api/chats', 'POST', {})
  workspace.service.store.put('chats', { ...chat, title: 'Finalize the October release' })
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('An unsent draft to remove')
  const openConversations = async () => {
    await page.getByRole('button', { name: 'Chat details', exact: true }).click()
    await page.getByRole('dialog', { name: 'Chat details', exact: true }).getByRole('button', { name: 'Search conversations', exact: true }).click()
  }

  const dialog = page.getByRole('dialog', { name: /Conversations/ })
  const confirmation = page.getByRole('dialog', { name: 'Delete conversation?', exact: true })
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  const details = page.getByRole('dialog', { name: 'Chat details', exact: true })
  await expect(details.getByRole('button', { name: 'Delete conversation', exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('delete-chat-details-1280.png'), animations: 'disabled' })
  await details.getByRole('button', { name: 'Delete conversation', exact: true }).click()
  await expect(confirmation).toBeVisible()
  await confirmation.getByRole('button', { name: 'Keep conversation' }).click()
  for (const [width, colorScheme] of [[1280, 'light'], [390, 'dark']] as const) {
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

  // Codex reports the latest request (40,900 tokens), not the thread's cumulative total.
  const indicator = page.getByTestId('context-indicator')
  await expect(indicator).toHaveText('Context 41K / 258K · 16%')
  await expect(page.getByText('Ready for the next step.')).toBeVisible()
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  const details = page.getByRole('dialog', { name: 'Chat details', exact: true })
  await expect(details.getByTestId('context-usage')).toContainText('40,900 of 258,400 tokens (16%)')
  await expect(details.getByTestId('context-usage')).toContainText('36,000 tokens read from cache')
  await page.screenshot({ path: testInfo.outputPath('context-details.png'), animations: 'disabled' })
  await details.getByRole('button', { name: 'Compact context', exact: true }).click()
  const confirmation = page.getByRole('dialog', { name: 'Compact the agent context?', exact: true })
  await expect(confirmation).toContainText('Every message stays visible')
  await confirmation.getByRole('button', { name: 'Compact context', exact: true }).click()
  await expect(confirmation).toHaveCount(0)

  const divider = page.getByTestId('context-compaction')
  await expect(divider).toHaveCount(1)
  await expect(divider).toContainText('Context compacted', { timeout: 15000 })
  await expect(indicator).toHaveText('Context ≈ 6K / 258K · 2%')
  // The earlier reply and request stay in the transcript; no reply or new finish is invented.
  await expect(page.getByText('Ready for the next step.')).toBeVisible()
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
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  await expect(details.getByTestId('context-usage')).toContainText('Estimated by the coding agent')
  await expect(details.getByRole('button', { name: 'Compact context', exact: true })).toBeEnabled()
  await page.screenshot({ path: testInfo.outputPath('context-details-phone.png'), animations: 'disabled' })
})

test('compaction is unavailable while a turn runs and explains why', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'fixture:chat-hang' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 15000 }).toBe('running')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: 'Chat details', exact: true }).click()
  const details = page.getByRole('dialog', { name: 'Chat details', exact: true })
  await expect(details.getByRole('button', { name: 'Compact context', exact: true })).toBeDisabled()
  await expect(details).toContainText('Wait for the current turn to finish before compacting.')
  await expect(workspace.api(`/api/chats/${chat.id}/compact`, 'POST', {})).rejects.toThrow(/Wait for the current turn to finish/)
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
})
