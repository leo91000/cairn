import { randomUUID } from 'node:crypto'
import {
  expect,
  signIn,
  test,
  useRelayForHttpMocks,
  workspacePath,
} from './fixtures'
import { scheduleDefinition } from './schedule-fixture'

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
  await dialog.getByRole('button', { name: 'Preview next runs' }).click()
  await expect(dialog.getByRole('list', { name: 'Upcoming runs' }).getByRole('listitem')).toHaveCount(5)
  await dialog.getByRole('button', { name: 'Create schedule' }).click()
  await expect(dialog).toHaveCount(0)
  const rule = (await workspace.api('/api/hook-rules')).rules.find((rule: { name: string }) => rule.name === 'Daily dependency report')
  expect(rule.trigger).toMatchObject({ type: 'clock', timezone: 'Europe/Paris' })
  expect(rule.nextRun).toBeGreaterThan(Date.now())
  await expect(page.locator('time[aria-label="Next run"]')).toHaveAttribute('datetime', new Date(rule.nextRun).toISOString())
  await page.getByRole('button', { name: 'Run now' }).click()
  await expect(page.getByRole('button', { name: 'History', exact: true })).toHaveAttribute('aria-pressed', 'true')

  const scheduledChat = async () => (await workspace.api('/api/chats')).find((chat: any) => chat.originRule?.ruleId === rule.id)
  await expect.poll(async () => (await scheduledChat())?.status, { timeout: 30000 }).toBe('succeeded')
  const chat = await scheduledChat()
  expect(chat.originRule).toMatchObject({ ruleId: rule.id, schedule: true })
  expect(chat.unread).toBe(false)
  const finished = page.getByRole('button', { name: /Completed.*1 action/i })
  await finished.click()
  await expect(page.getByRole('link', { name: 'Open conversation', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'Run now' }).click()
  const nextChat = async () => (await workspace.api('/api/chats')).find((entry: any) => entry.originRule?.ruleId === rule.id && entry.id !== chat.id)
  await expect.poll(async () => (await nextChat())?.status, { timeout: 30000 }).toBe('succeeded')
  const next = await nextChat()
  await expect(finished).toHaveCount(2)
  await finished.first().click()
  await page.getByRole('link', { name: 'Open conversation', exact: true }).click()
  await expect(page).toHaveURL(workspacePath(`/chats/${next.id}`))
  await expect(page.getByRole('textbox', { name: 'Message' })).toBeVisible()
  await workspace.api(`/api/chats/${next.id}/messages`, 'POST', { id: randomUUID(), text: 'Summarize the changes in one line.' })
  await expect.poll(async () => (await nextChat())?.personParticipated).toBe(true)
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

for (const savedCron of ['30 13 * * MON', '0 9 * * *']) {
  test(`accepting a concurrent version resets the editable schedule to ${savedCron}`, async ({ page, workspace }) => {
    const agent = workspace.service.agent({ name: 'Scheduler' })
    const definition = scheduleDefinition('Weekly triage', agent.id, 'Review the current workspace.')
    const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: false })
    await page.goto(workspacePath(`/rules/${rule.id}/edit`))
    await signIn(page)
    const cron = page.getByLabel('Cron (five fields)', { exact: true })
    await expect(cron).toHaveValue('0 9 * * *')

    const path = `/api/hook-rules/${rule.id}`
    const changed = { ...definition, trigger: { ...definition.trigger, cron: savedCron } }
    await workspace.api(path, 'PUT', { ruleId: rule.id, revision: rule.revision, definition: changed })
    await cron.fill('15 10 * * *')
    await page.getByRole('button', { name: 'Save version' }).click()
    await page.getByRole('button', { name: 'Use the saved version' }).click()
    await expect(cron).toHaveValue(savedCron)

    await page.getByLabel('Time zone').fill('UTC')
    await page.getByRole('button', { name: 'Save version' }).click()
    await expect.poll(async () => (await workspace.api(path)).definition.trigger).toEqual({
      type: 'clock',
      cron: savedCron,
      timezone: 'UTC',
    })
  })
}

test('accepting a completed save discards Clock changes made while it was in flight', async ({ page, workspace }) => {
  await useRelayForHttpMocks(page)
  const agent = workspace.service.agent({ name: 'Scheduler' })
  const definition = scheduleDefinition('Clock in flight', agent.id, 'Review work.')
  const rule = await workspace.api('/api/hook-rules', 'POST', { definition, enabled: false })
  await page.goto(workspacePath(`/rules/${rule.id}/edit`))
  await signIn(page)
  const cron = page.getByLabel('Cron (five fields)', { exact: true })
  await expect(cron).toHaveValue('0 9 * * *')

  let held = false
  let confirmSaved!: () => void
  let releaseResponse!: () => void
  const saved = new Promise<void>(resolve => confirmSaved = resolve)
  const release = new Promise<void>(resolve => releaseResponse = resolve)
  await page.route(`**/api/hook-rules/${rule.id}`, async (route) => {
    if (held || route.request().method() !== 'PUT')
      return route.continue()

    held = true
    const response = await route.fetch()
    confirmSaved()
    await release
    await route.fulfill({ response })
  })

  try {
    await page.getByRole('button', { name: 'Save version' }).click()
    await saved
    await cron.fill('15 10 * * *')
  }
  finally {
    releaseResponse()
  }

  await page.getByRole('button', { name: 'Use the saved version' }).click()
  await expect(cron).toHaveValue('0 9 * * *')
  await page.getByLabel('Time zone').fill('UTC')
  await page.getByRole('button', { name: 'Save version' }).click()
  await expect.poll(async () => (await workspace.api(`/api/hook-rules/${rule.id}`)).definition.trigger).toEqual({
    type: 'clock',
    cron: '0 9 * * *',
    timezone: 'UTC',
  })
})
