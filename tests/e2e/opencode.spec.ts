import {
  expect,
  expectSingleScroll,
  initializeRepository,
  signIn,
  test,
  workspacePath,
} from './fixtures'

test('OpenCode connects a Zen key, selects native models and runs a conversation', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  initializeRepository(workspace.projectPath)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(workspacePath('/connections'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Add an account' })
  await dialog.getByRole('radio', { name: /OpenCode/ }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('OpenCode Zen')
  await expect(dialog.getByLabel('Model provider')).toContainText('OpenCode Zen')
  await expect(dialog.getByRole('button', { name: 'Save API key' })).toBeDisabled()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`opencode-connect-${width}.png`), animations: 'disabled' })
  }

  await dialog.getByLabel('API key', { exact: true }).fill('synthetic-browser-zen-key')
  await dialog.getByRole('button', { name: 'Save API key' }).click()
  await expect(dialog).toHaveCount(0)
  const accounts = (await workspace.api('/api/accounts')).accounts
  const account = accounts.find((a: { provider: string }) => a.provider === 'opencode')
  expect(account).toMatchObject({
    name: 'OpenCode Zen',
    modelProvider: 'opencode',
    modelProviderName: 'OpenCode Zen',
    state: 'ready',
  })
  expect(JSON.stringify(accounts)).not.toContain('synthetic-browser-zen-key')
  await expect(page.getByRole('region', { name: 'OpenCode accounts' })).toContainText('OpenCode Zen')

  await page.goto(workspacePath('/agents'))
  await page.getByRole('button', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('OpenCode workspace helper')
  const picker = page.getByRole('button', { name: /^Agent, model and reasoning/ })
  await picker.click()
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  await menu.getByRole('radio', { name: 'OpenCode', exact: true }).click()
  await expect(menu.getByRole('radio', { name: 'Ling 3.1 Flash Free · OpenCode Zen', exact: true })).toBeVisible()
  await menu.getByRole('radio', { name: 'Ling 3.1 Flash Free · OpenCode Zen', exact: true }).click()
  await menu.getByRole('radiogroup', { name: 'Reasoning' }).getByRole('radio', { name: 'High', exact: true }).click()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`opencode-models-${width}.png`), animations: 'disabled' })
  }

  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await page.getByRole('button', { name: 'Save agent', exact: true }).click()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  const agent = (await workspace.api('/api/agents')).find((a: { name: string }) => a.name === 'OpenCode workspace helper')!
  expect(agent).toMatchObject({ provider: 'opencode', model: 'opencode/ling-3.1-flash-free', reasoning: 'high' })
  await page.locator('article').filter({ has: page.getByRole('button', { name: 'OpenCode workspace helper', exact: true }) }).getByRole('link', { name: 'Start chat' }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Prepare a compact workspace status panel.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  const slug = new URL(page.url()).pathname.split('/').at(-1)!
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  await expect(page.getByText(/I checked the workspace and prepared a compact status panel/).last()).toBeVisible()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`opencode-conversation-${width}.png`), animations: 'disabled' })
  }

  // Resume the same native session for a second request.
  const previous = await workspace.api(`/api/chats/${slug}`)
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Now add the project status.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run?.summary, { timeout: 30000 }).toContain('compact status panel')
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run.resumeCount, { timeout: 30000 }).toBeGreaterThan(previous.run.resumeCount ?? 0)
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run.status, { timeout: 30000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${slug}`)).run.sessionId).toBe(previous.run.sessionId)
  expect(errors).toEqual([])
})

test('OpenCode answers native questions, steers a busy session and replaces an idle key', async ({ workspace }) => {
  test.setTimeout(90000)
  initializeRepository(workspace.projectPath)
  let account = (await workspace.api('/api/accounts')).accounts.find((a: { provider: string }) => a.provider === 'opencode')
  if (!account) {
    account = await workspace.api('/api/accounts', 'POST', {
      provider: 'opencode',
      name: 'Zen test',
      modelProvider: 'opencode',
      apiKey: 'synthetic-browser-zen-key',
    })
  }

  const chat = await workspace.api('/api/chats', 'POST', {})
  const model = 'opencode/ling-3.1-flash-free'
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: crypto.randomUUID(),
    provider: 'opencode',
    model,
    text: 'fixture:opencode-question',
  })
  try {
    await expect.poll(async () => {
      const detail = await workspace.api(`/api/chats/${chat.id}`)
      return detail.questions.filter((q: { status: string }) => q.status === 'pending').length
    }, { timeout: 30000 }).toBe(1)
  }
  catch (error) {
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    throw new Error(JSON.stringify({
      paused: detail.paused,
      error: detail.error,
      eventKinds: detail.run ? (await workspace.api(`/api/runs/${detail.run.id}/events`)).map((e: { type: string }) => e.type) : [],
      run: detail.run && { status: detail.run.status, error: detail.run.error, wait: detail.run.accountWaitReason },
      messages: detail.messages.map((m: { status: string, provider: string, model: string }) => ({ status: m.status, provider: m.provider, model: m.model })),
    }), { cause: error })
  }

  const question = (await workspace.api(`/api/chats/${chat.id}`)).questions.find((q: { status: string }) => q.status === 'pending')
  await workspace.api(`/api/chats/${chat.id}/questions/${question.id}/answer`, 'POST', { id: crypto.randomUUID(), answers: { 0: ['Compact'] } })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.summary).toContain('Compact layout')
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'fixture:opencode-hang' })
  await expect.poll(async () => {
    const detail = await workspace.api(`/api/chats/${chat.id}`)
    if (detail.error)
      throw new Error(detail.error)
    return detail.run?.status
  }, { timeout: 30000 }).toBe('running')
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Finish with the compact status panel', mode: 'steer' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  const replaced = await workspace.api(`/api/accounts/${account.id}/api-key`, 'POST', { modelProvider: 'opencode', apiKey: 'synthetic-browser-replacement-key' })
  expect(replaced.id).toBe(account.id)
  expect(JSON.stringify(replaced)).not.toContain('synthetic-browser-replacement-key')
})
