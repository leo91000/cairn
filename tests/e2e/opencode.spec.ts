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
  // One entry per model; a single connection needs no choice.
  await expect(menu.getByRole('radio', { name: 'Ling 3.1 Flash Free', exact: true })).toBeVisible()
  await menu.getByRole('radio', { name: 'Ling 3.1 Flash Free', exact: true }).click()
  await expect(menu.getByText('Via OpenCode Zen · Credits')).toBeVisible()
  await expect(menu.getByRole('radiogroup', { name: 'Connection' })).toHaveCount(0)
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
  expect(agent).toMatchObject({ provider: 'opencode', model: 'auto/ling-3-1-flash-free', reasoning: 'high' })
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
  let account = (await workspace.api('/api/accounts')).accounts.find((a: { modelProvider?: string }) => a.modelProvider === 'opencode')
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

test('OpenCode Auto runs a model on the first connection, then the next one at its limit', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  initializeRepository(workspace.projectPath)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  const existing = (await workspace.api('/api/accounts')).accounts
  const zen = existing.find((a: { modelProvider?: string }) => a.modelProvider === 'opencode') ?? await workspace.api('/api/accounts', 'POST', {
    provider: 'opencode',
    name: 'OpenCode Zen',
    modelProvider: 'opencode',
    apiKey: 'synthetic-browser-zen-key',
  })
  const go = existing.find((a: { modelProvider?: string }) => a.modelProvider === 'opencode-go') ?? await workspace.api('/api/accounts', 'POST', {
    provider: 'opencode',
    name: 'OpenCode Go',
    modelProvider: 'opencode-go',
    apiKey: 'synthetic-browser-go-key',
  })
  const ordered = (await workspace.api('/api/accounts')).accounts.filter((a: { provider: string }) => a.provider === 'opencode').map((a: { id: string }) => a.id)
  await workspace.api('/api/accounts/order', 'POST', { provider: 'opencode', ids: [go.id, ...ordered.filter((id: string) => id !== go.id)] })

  // The same model through Go and Zen is one entry with both routes, Go first.
  const catalog = await workspace.api('/api/opencode/models')
  const ling = catalog.models.find((m: { model: string }) => m.model === 'auto/ling-3-1-flash-free')
  expect(ling.routes.map((r: { model: string, billing: string }) => [r.model, r.billing])).toEqual([
    ['opencode-go/ling-3.1-flash-free', 'subscription'],
    ['opencode/ling-3.1-flash-free', 'credits'],
  ])
  expect(ling.supportedReasoningEfforts.map((e: { reasoningEffort: string }) => e.reasoningEffort)).toEqual(['medium', 'high'])
  expect(catalog.models.some((m: { routes: { provider: string }[] }) => m.routes.some(r => r.provider === 'openrouter'))).toBe(false)

  await page.goto(workspacePath('/connections'))
  await signIn(page)
  const region = page.getByRole('region', { name: 'OpenCode accounts' })
  await expect(region.getByRole('button', { name: /^OpenCode Go, / })).toBeVisible()
  await expect(region.getByRole('button', { name: 'Move OpenCode Go up' })).toBeDisabled()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await region.screenshot({ path: testInfo.outputPath(`opencode-order-${width}.png`), animations: 'disabled' })
  }

  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: crypto.randomUUID(),
    provider: 'opencode',
    model: 'auto/ling-3-1-flash-free',
    text: 'fixture:opencode-limit Prepare a compact workspace status panel.',
  })
  await expect.poll(async () => {
    const { run } = await workspace.api(`/api/chats/${chat.id}`)
    if (run?.status === 'failed') {
      const events = await workspace.api(`/api/runs/${run.id}/events`)
      throw new Error(JSON.stringify({ error: run.error, events: events.map((e: { type: string, text: string }) => `${e.type}: ${e.text}`.slice(0, 200)) }))
    }

    return run?.status
  }, { timeout: 60000 }).toBe('succeeded')
  const detail = await workspace.api(`/api/chats/${chat.id}`)
  const statuses = (await workspace.api(`/api/runs/${detail.run.id}/events`)).filter((e: { type: string }) => e.type === 'status').map((e: { text: string }) => e.text)
  expect(statuses).toContain('Ling 3.1 Flash Free via OpenCode Go')
  expect(statuses.indexOf('Ling 3.1 Flash Free via OpenCode Zen')).toBeGreaterThan(statuses.indexOf('Ling 3.1 Flash Free via OpenCode Go'))
  expect(detail.run.accountId).toBe(zen.id)

  await page.goto(workspacePath(`/chats/${chat.id}`))
  // The reply names the connection that wrote it.
  await expect(page.getByRole('article').filter({ hasText: 'compact status panel' }).getByText('via OpenCode Zen')).toBeVisible()
  const picker = page.getByRole('button', { name: /^Agent, model and reasoning/ })
  await picker.click()
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  const connection = menu.getByRole('radiogroup', { name: 'Connection' })
  await expect(connection.getByRole('radio', { name: 'Auto' })).toHaveAttribute('aria-checked', 'true')
  await expect(menu.getByText('Tries OpenCode Go, then OpenCode Zen, in your Connections order.')).toBeVisible()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`opencode-routes-${width}.png`), animations: 'disabled' })
  }

  // Pinning a connection keeps the model and shows the provider beside it.
  await connection.getByRole('radio', { name: /^OpenCode Zen/ }).click()
  await expect(connection.getByRole('radio', { name: /^OpenCode Zen/ })).toHaveAttribute('aria-checked', 'true')
  await expect(menu.getByText('Runs only on OpenCode Zen. When its limit is reached, the run waits.')).toBeVisible()
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await expect(picker).toHaveAccessibleName(/Ling 3\.1 Flash Free · OpenCode Zen/)
  expect(errors).toEqual([])
})
