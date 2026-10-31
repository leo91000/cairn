import { randomUUID } from 'node:crypto'
import { connectClaude } from './accounts'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
  workspacePath,
} from './fixtures'

// The fixture's first reply of a turn, which side questions refer to.
const reply = 'I’ll trace the component boundaries and keyboard handling'

// Only Claude Code answers side questions. The account belongs to this worker's workspace.
test.beforeAll(async ({ workspace }) => {
  const { accounts } = await workspace.api('/api/accounts')
  if (!accounts.some((account: { provider: string }) => account.provider === 'claude'))
    await connectClaude(workspace, 'Side questions', 'fixture-code:side-questions@example.test')
})

test('asks side questions about a running reply without steering, queuing or recording them', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the layout. fixture:chat-hang', provider: 'claude' })
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const message = page.locator('.activity-message').filter({ hasText: reply })
  await expect(message).toBeVisible()

  // The whole reply: its header action opens the side panel.
  await message.getByRole('button', { name: 'Ask about this reply' }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await expect(panel.getByText('Claude Code answers from the conversation so far, without tools')).toBeVisible()
  await expect(panel.getByRole('textbox', { name: 'Your question about this reply' })).toBeFocused()
  await panel.getByRole('textbox', { name: 'Your question about this reply' }).fill('Why trace the boundaries first?')
  await page.screenshot({ path: test.info().outputPath('side-question-desktop-light.png'), animations: 'disabled' })
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  // The agent received the start of the reply with the question (the fixture echoes it).
  await expect(panel.getByText(/Side answer: About this reply of yours:/)).toBeVisible()
  await expectSingleScroll(page)
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: test.info().outputPath('side-question-answer-desktop-dark.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'light' })

  // Following the answer back to its reply closes the panel; the reply counts it.
  await panel.getByRole('button', { name: 'Show reply' }).click()
  await expect(panel).not.toBeVisible()
  await expect(message.getByRole('button', { name: 'Show 1 side question about this reply' })).toBeVisible()

  // A passage selected in the reply can be asked about too.
  await message.locator('.markdown p').first().evaluate((paragraph) => {
    const text = paragraph.firstChild!
    const range = document.createRange()
    range.setStart(text, 5)
    range.setEnd(text, 35)
    document.getSelection()!.removeAllRanges()
    document.getSelection()!.addRange(range)
  })
  await page.getByRole('button', { name: 'Ask about selection' }).click()
  await expect(panel.getByText('trace the component boundaries', { exact: true })).toBeVisible()
  await panel.getByRole('textbox', { name: 'Your question about this passage' }).fill('Which boundaries?')
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect(panel.getByText(/Side answer: About this passage of your reply:/)).toBeVisible()
  await panel.getByRole('button', { name: 'Close dialog' }).click()

  // Nothing joined the queue or the conversation, and the reply keeps running.
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(detail.run.status).toBe('running')
  // Only the turn's own message, which this fixture never acknowledges as delivered.
  expect(detail.messages.map((queued: { text: string }) => queued.text)).toEqual(['Review the layout. fixture:chat-hang'])
  expect(detail.sideQuestions.map((question: { status: string }) => question.status)).toEqual(['answered', 'answered'])
  expect(detail.sideQuestions[1].quote).toBe('trace the component boundaries')
  expect(detail.sideQuestions[0].itemId).toBe(detail.sideQuestions[1].itemId)
  const events = JSON.stringify(workspace.service.store.events(detail.run.id, 0, 200))
  for (const side of ['Why trace the boundaries first?', 'Which boundaries?', 'Side answer'])
    expect(events).not.toContain(side)

  // Steering still reaches the turn, which then finishes; side questions wait for the next reply.
  const header = message.locator('p').first()
  const running = (await header.boundingBox())!.height
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('finish now')
  await page.getByRole('button', { name: 'Steer now' }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  await expect(message.getByRole('button', { name: 'Ask about this reply' })).toHaveCount(0)
  // The action leaving does not move the conversation, which keeps following its end.
  expect((await header.boundingBox())!.height).toBe(running)
  await message.getByRole('button', { name: 'Show 2 side questions about this reply' }).click()
  await expect(panel.getByText('Side questions are answered while the agent works on a reply. Send a message instead.')).toBeVisible()
  await expect(panel.getByText('About one reply')).toBeVisible()

  // Phones show the same panel as a bottom sheet.
  await page.setViewportSize({ width: 390, height: 844 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('side-question-mobile-light.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: test.info().outputPath('side-question-mobile-dark.png'), animations: 'disabled' })
})

test('keeps a reply filter when asking more than once', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review. fixture:chat-hang', provider: 'claude' })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const message = page.locator('.activity-message').filter({ hasText: reply })
  await message.getByRole('button', { name: 'Ask about this reply' }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await panel.getByRole('textbox', { name: 'Your question about this reply' }).fill('First question')
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect(panel.getByText('First question', { exact: true })).toBeVisible()
  await panel.getByRole('button', { name: 'Close dialog' }).click()
  await message.getByRole('button', { name: 'Show 1 side question about this reply' }).click()
  await panel.getByRole('textbox', { name: 'Your question about this reply' }).fill('Second question')
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect(panel.getByText('Second question', { exact: true })).toBeVisible()
  await panel.getByRole('textbox', { name: 'Your question about this reply' }).fill('Third question')
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect(panel.getByText('Third question', { exact: true })).toBeVisible()
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(new Set(detail.sideQuestions.map((question: { itemId: string }) => question.itemId)).size).toBe(1)
  expect(detail.sideQuestions[0].itemId).not.toBe('')
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST', {})
})

test('links reused native item IDs to the selected reply', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review. fixture:chat-hang', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).canAskSideQuestion).toBe(true)
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const run = detail.run.id
  workspace.service.store.event(run, 'turn.started', '')
  workspace.service.store.event(run, 'item.updated', '', { item: { id: 'reused-reply', type: 'agent_message', text: 'First draft' } })
  workspace.service.store.event(run, 'item.completed', '', { item: { id: 'reused-reply', type: 'agent_message', text: 'First distinct reply' } })
  workspace.service.store.event(run, 'turn.started', '')
  workspace.service.store.event(run, 'item.completed', '', { item: { id: 'reused-reply', type: 'agent_message', text: 'Second distinct reply' } })
  await expect(workspace.api(`/api/chats/${chat.id}/side-questions`, 'POST', {
    id: randomUUID(),
    text: 'Which reply?',
    itemId: 'reused-reply',
  })).rejects.toThrow(/ambiguous/)
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const first = page.locator('.activity-message').filter({ hasText: 'First distinct reply' })
  const second = page.locator('.activity-message').filter({ hasText: 'Second distinct reply' })
  await expect(first).toBeVisible()
  await expect(second).toBeVisible()
  await first.getByRole('button', { name: 'Ask about this reply' }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await panel.getByRole('textbox', { name: 'Your question about this reply' }).fill('Why the first reply?')
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).sideQuestions[0]?.answer).toContain('First distinct reply')
  await panel.getByRole('button', { name: 'Show reply' }).click()
  await expect(first.getByRole('button', { name: 'Show 1 side question about this reply' })).toBeVisible()
  await expect(second.getByRole('button', { name: /Show .* side question/ })).toHaveCount(0)
  await page.reload()
  await expect(first.getByRole('button', { name: 'Show 1 side question about this reply' })).toBeVisible()
  await expect(second.getByRole('button', { name: /Show .* side question/ })).toHaveCount(0)
  await first.getByRole('button', { name: 'Show 1 side question about this reply' }).click()
  await expect(panel.getByText('Why the first reply?', { exact: true }).first()).toBeVisible()
  await panel.getByRole('button', { name: 'Show reply' }).click()
  await expect(panel).not.toBeVisible()
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})

test('retained questions without a reply stay accessible after reopening', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review. fixture:chat-hang', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).canAskSideQuestion).toBe(true)
  await workspace.api(`/api/chats/${chat.id}/side-questions`, 'POST', { id: randomUUID(), text: 'General question' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).sideQuestions[0]?.status).toBe('answered')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await page.getByRole('button', { name: /Side question · Saved/ }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await expect(panel.getByText('General question', { exact: true })).toBeVisible()
  await panel.getByRole('button', { name: 'Close dialog' }).click()
  await page.reload()
  await page.getByRole('button', { name: /Side question · Saved/ }).click()
  await expect(panel.getByText('General question', { exact: true })).toBeVisible()
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})

test('a pending side question can be withdrawn while the reply continues', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the layout. fixture:chat-hang', provider: 'claude' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).canAskSideQuestion).toBe(true)
  const asked = await workspace.api(`/api/chats/${chat.id}/side-questions`, 'POST', { id: randomUUID(), text: 'fixture:side-slow Why this order?' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  // The card's button is named by its visible text.
  await page.getByRole('button', { name: /^Side question · Claude Code is answering/ }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await expect(panel.getByText('Answering…')).toBeVisible()
  await panel.getByRole('button', { name: 'Withdraw' }).click()
  await expect(panel.getByText('Withdrawn')).toBeVisible()
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(detail.sideQuestions[0]).toMatchObject({ id: asked.id, status: 'cancelled' })
  expect(detail.run.status).toBe('running')
  // Frees the workspace's runner for the next journey.
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})

test('a Codex reply explains that it cannot take side questions', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the layout. fixture:chat-hang', provider: 'codex' })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const message = page.locator('.activity-message').filter({ hasText: reply })
  await expect(message).toBeVisible()
  await expect(message.getByRole('button', { name: 'Ask about this reply' })).toHaveCount(0)
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(detail.run.status).toBe('running')
  expect(detail.canAskSideQuestion).toBe(false)
  expect(detail.sideQuestionReason).toContain('Codex cannot answer side questions')
  await expect(workspace.api(`/api/chats/${chat.id}/side-questions`, 'POST', { id: randomUUID(), text: 'Why?' })).rejects.toThrow(/Codex cannot answer/)
  await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
})
