import { randomUUID } from 'node:crypto'
import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'

test('a schedule opens an ordinary conversation that can accept a follow-up', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await page.setViewportSize({ width: 390, height: 844 })
  await page.goto(workspacePath('/rules'))
  await signIn(page)
  await page.getByRole('button', { name: 'Schedule an agent' }).click()
  const dialog = page.getByRole('dialog', { name: 'Schedule an agent' })
  await dialog.getByLabel('Name', { exact: true }).fill('Daily dependency report')
  await dialog.getByLabel('Brief').fill('Review the current workspace.')
  await dialog.getByLabel('Time zone').fill('Europe/Paris')
  await dialog.getByRole('button', { name: 'Create schedule' }).click()
  await expect(dialog).toHaveCount(0)
  const rule = (await workspace.api('/api/hook-rules')).rules.find((rule: { name: string }) => rule.name === 'Daily dependency report')
  expect(rule.trigger).toMatchObject({ type: 'clock', timezone: 'Europe/Paris' })
  await page.getByRole('button', { name: 'Run now' }).click()
  await expect(page.getByRole('button', { name: 'History', exact: true })).toHaveAttribute('aria-pressed', 'true')

  const scheduledChat = async () => (await workspace.api('/api/chats')).find((chat: any) => chat.originRule?.ruleId === rule.id)
  await expect.poll(async () => (await scheduledChat())?.status, { timeout: 30000 }).toBe('succeeded')
  const chat = await scheduledChat()
  expect(chat.originRule).toMatchObject({ ruleId: rule.id, schedule: true })
  expect(chat.unread).toBe(false)
  await page.goto(workspacePath(`/chats/${chat.slug}`))
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: randomUUID(), text: 'Summarize the changes in one line.' })
  await expect.poll(async () => (await scheduledChat())?.personParticipated).toBe(true)
})

test('retrying an uncertain creation keeps one schedule', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  await page.goto(workspacePath('/rules'))
  await signIn(page)
  await page.getByRole('button', { name: 'Schedule an agent' }).click()
  const dialog = page.getByRole('dialog', { name: 'Schedule an agent' })
  await dialog.getByLabel('Name', { exact: true }).fill('Retry daily report')
  await dialog.getByLabel('Brief').fill('Review the workspace.')
  let dropped = false
  await page.route('**/api/hook-rules', async (route) => {
    if (dropped || route.request().method() !== 'POST')
      return route.continue()

    const response = await route.fetch()
    expect(response.ok()).toBe(true)
    dropped = true
    await route.abort()
  })
  await dialog.getByRole('button', { name: 'Create schedule' }).click()
  await expect(dialog.getByRole('alert')).toBeVisible()
  await dialog.getByLabel('Brief').fill('Review the new workspace.')
  await dialog.getByRole('button', { name: 'Create schedule' }).click()
  await expect(dialog).toHaveCount(0)
  const rules = (await workspace.api('/api/hook-rules')).rules.filter((rule: { name: string }) => rule.name === 'Retry daily report')
  expect(rules).toHaveLength(1)
  const saved = await workspace.api(`/api/hook-rules/${rules[0].id}`)
  expect(saved.definition.steps[0].then[0].action.prompt.value).toBe('Review the new workspace.')
})
