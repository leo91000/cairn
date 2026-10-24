import { randomUUID } from 'node:crypto'
import {
  signIn as authenticateWorkspace,
  expect,
  expectSingleScroll,
  test,
  workspacePath,
} from './fixtures'

// The fixture's first reply of a turn, which side questions refer to.
const reply = 'I’ll trace the component boundaries and keyboard handling'

test('asks side questions about a running reply without steering, queuing or recording them', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the layout. fixture:chat-hang' })
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  const message = page.locator('.activity-message').filter({ hasText: reply })
  await expect(message).toBeVisible()

  // The whole reply: its header action opens the side panel.
  await message.getByRole('button', { name: 'Ask about this reply' }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await expect(panel.getByText('Codex answers from the conversation so far, without tools')).toBeVisible()
  await panel.getByRole('textbox', { name: 'Your question about this reply' }).fill('Why trace the boundaries first?')
  await page.screenshot({ path: test.info().outputPath('side-question-desktop-light.png'), animations: 'disabled' })
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect(panel.getByText('Side answer: Why trace the boundaries first?')).toBeVisible()
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
  await expect(panel.getByText('trace the component boundaries')).toBeVisible()
  await panel.getByRole('textbox', { name: 'Your question about this passage' }).fill('Which boundaries?')
  await panel.getByRole('button', { name: 'Ask', exact: true }).click()
  await expect(panel.getByText(/Side answer: About this passage of your reply:/)).toBeVisible()
  await panel.getByRole('button', { name: 'Close dialog' }).click()

  // Nothing joined the queue or the conversation, and the reply keeps running.
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(detail.run.status).toBe('running')
  expect(detail.messages).toHaveLength(0)
  expect(detail.sideQuestions.map((question: { status: string }) => question.status)).toEqual(['answered', 'answered'])
  expect(detail.sideQuestions[1].quote).toBe('trace the component boundaries')
  expect(detail.sideQuestions[0].itemId).toBe(detail.sideQuestions[1].itemId)
  const users = workspace.service.store.events(detail.run.id, 0, 200).filter(event => event.type === 'chat.user')
  expect(users).toHaveLength(1)
  expect(JSON.stringify(workspace.service.store.events(detail.run.id, 0, 200))).not.toContain('Side answer')

  // Steering still reaches the turn, which then finishes; side questions wait for the next reply.
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('finish now')
  await page.getByRole('button', { name: 'Steer now' }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  await expect(message.getByRole('button', { name: 'Ask about this reply' })).toHaveCount(0)
  await message.getByRole('button', { name: 'Show 2 side questions about this reply' }).click()
  await expect(panel.getByText('Side questions are answered while Codex or Claude Code works on a reply.')).toBeVisible()
  await expect(panel.getByText('About one reply')).toBeVisible()

  // Phones show the same panel as a bottom sheet.
  await page.setViewportSize({ width: 390, height: 844 })
  await expectSingleScroll(page)
  await page.screenshot({ path: test.info().outputPath('side-question-mobile-light.png'), animations: 'disabled' })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: test.info().outputPath('side-question-mobile-dark.png'), animations: 'disabled' })
})

test('a pending side question can be withdrawn while the reply continues', async ({ page, workspace }) => {
  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Review the layout. fixture:chat-hang' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).canAskSideQuestion).toBe(true)
  const asked = await workspace.api(`/api/chats/${chat.id}/side-questions`, 'POST', { id: randomUUID(), text: 'fixture:side-slow Why this order?' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await authenticateWorkspace(page)
  await expect(page.getByText('Side question · Codex is answering')).toBeVisible()
  await page.getByRole('button', { name: 'Open side questions' }).click()
  const panel = page.getByRole('dialog', { name: 'Side questions' })
  await expect(panel.getByText('Answering…')).toBeVisible()
  await panel.getByRole('button', { name: 'Withdraw' }).click()
  await expect(panel.getByText('Withdrawn')).toBeVisible()
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  expect(detail.sideQuestions[0]).toMatchObject({ id: asked.id, status: 'cancelled' })
  expect(detail.run.status).toBe('running')
})
