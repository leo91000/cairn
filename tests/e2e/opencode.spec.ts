import {
  createAgent,
  expect,
  expectSingleScroll,
  grantFullAccess,
  initializeRepository,
  openAgentSection,
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
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('OpenCode workspace helper')
  await grantFullAccess(page)
  await openAgentSection(page, 'Coding agent & model')
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
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
  await createAgent(page)
  const agent = (await workspace.api('/api/agents')).find((a: { name: string }) => a.name === 'OpenCode workspace helper')!
  expect(agent).toMatchObject({ provider: 'opencode', model: 'auto/ling-3-1-flash-free', reasoning: 'high' })
  await page.getByRole('link', { name: 'Chat with this agent' }).click()
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

test('OpenCode answers common questionnaires, steers a busy session and replaces an idle key', async ({ workspace }) => {
  test.setTimeout(90000)
  initializeRepository(workspace.projectPath)
  let account = (await workspace.api('/api/accounts')).accounts.find((a: { modelProvider?: string }) => a.modelProvider === 'opencode')
  if (!account) {
    account = (await workspace.api('/api/accounts', 'POST', {
      provider: 'opencode',
      name: 'Zen test',
      modelProvider: 'opencode',
      apiKey: 'synthetic-browser-zen-key',
    })).account
  }

  const chat = await workspace.api('/api/chats', 'POST', {})
  const model = 'opencode/ling-3.1-flash-free'
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: crypto.randomUUID(),
    provider: 'opencode',
    model,
    text: 'fixture:cairn-question',
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
  await workspace.api(`/api/chats/${chat.id}/questions/${question.id}/answer`, 'POST', { id: crypto.randomUUID(), answers: { layout: { selected: ['compact'], comment: 'Compact layout' } } })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).questions.find((q: { id: string }) => q.id === question.id)?.status).toBe('answered')
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.summary).toContain('compact status panel')
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
  expect(replaced.account.id).toBe(account.id)
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
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
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

test('OpenCode keeps an explicitly selected model and reasoning after opening and reloading a new chat', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  const accounts = (await workspace.api('/api/accounts')).accounts
  if (!accounts.some((account: { modelProvider?: string }) => account.modelProvider === 'opencode')) {
    await workspace.api('/api/accounts', 'POST', {
      provider: 'opencode',
      name: 'Model continuity',
      modelProvider: 'opencode',
      apiKey: 'synthetic-model-continuity-key',
    })
  }

  await page.goto(workspacePath('/chats'))
  await signIn(page)
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  await picker.click()
  await menu.getByRole('radio', { name: 'OpenCode', exact: true }).click()
  await menu.getByRole('radio', { name: 'Cat Jam Free', exact: true }).click()
  await menu.getByRole('radiogroup', { name: 'Reasoning' }).getByRole('radio', { name: 'High', exact: true }).click()
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Check the workspace status.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  const slug = new URL(page.url()).pathname.split('/').at(-1)!
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  await expect(picker).toHaveAccessibleName(/OpenCode · Cat Jam Free · High reasoning$/)
  await page.reload()
  await expect(picker).toHaveAccessibleName(/OpenCode · Cat Jam Free · High reasoning$/)
  await picker.click()
  await expect(menu.getByRole('radio', { name: 'Conversation model', exact: true })).toHaveAttribute('aria-checked', 'true')
  await expect(menu.getByText('Models from your connections')).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.screenshot({ path: testInfo.outputPath('opencode-catalog-desktop.png'), animations: 'disabled' })
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.screenshot({ path: testInfo.outputPath('opencode-catalog-mobile.png'), animations: 'disabled' })
  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Continue with the same model.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).messages.at(-1)).toMatchObject({ provider: 'opencode', model: 'auto/cat-jam-free', reasoning: 'high' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run?.status, { timeout: 30000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${slug}`)).run.snapshot.agent).toMatchObject({ model: 'auto/cat-jam-free', reasoning: 'high' })
})

test('OpenCode explains a regional Privacy refusal and offers retry or a new conversation', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  // The preceding limit journey intentionally exhausts its Go account.
  await workspace.api('/api/accounts', 'POST', {
    provider: 'opencode',
    name: 'Privacy example',
    modelProvider: 'opencode-go',
    apiKey: 'synthetic-privacy-key',
  })

  const chat = await workspace.api('/api/chats', 'POST', {})
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
    id: crypto.randomUUID(),
    provider: 'opencode',
    model: 'opencode-go/ling-3.1-flash-free',
    text: 'fixture:opencode-region Check the workspace status.',
  })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('failed')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await signIn(page)
  const notice = page.getByRole('alert').filter({ hasText: 'This model needs a Privacy change' })
  await expect(notice).toBeVisible()
  await expect(notice).toContainText('Global')
  await expect(notice.getByRole('link', { name: /OpenCode settings/ })).toHaveAttribute('href', 'https://opencode.ai/console/')
  await expect(page.getByText('Sensitive provider detail')).toHaveCount(0)
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`opencode-privacy-${width}.png`), animations: 'disabled' })
  }

  const previous = (await workspace.api(`/api/chats/${chat.id}`)).run
  await notice.getByRole('button', { name: 'Retry response', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.resumeCount, { timeout: 30000 }).toBeGreaterThan(previous.resumeCount ?? 0)
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status, { timeout: 30000 }).toBe('failed')
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.sessionId).toBe(previous.sessionId)
  await notice.getByRole('link', { name: 'choose another model in a new conversation', exact: true }).click()
  await expect(page.getByRole('textbox', { name: 'Message', exact: true })).toBeVisible()
  await expect(page).toHaveURL(/\/chats(?:\?|$)/)
  expect((await workspace.api(`/api/chats/${chat.id}`)).run.status).toBe('failed')
})

test('OpenCode Go displays three quotas, reset times and the provider balance setting', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  const apiKey = 'synthetic-browser-go-usage-key'
  const account = (await workspace.api('/api/accounts', 'POST', {
    provider: 'opencode',
    name: 'Go usage',
    modelProvider: 'opencode-go',
    apiKey,
  })).account
  await workspace.api('/api/accounts/refresh', 'POST')
  await page.goto(workspacePath('/connections'))
  await signIn(page)
  const row = page.getByRole('button', { name: /^Go usage, / })
  await expect(row.getByRole('progressbar', { name: 'Go usage 5-hour window remaining' })).toHaveAttribute('aria-valuenow', '75')
  await row.click()
  const panel = page.getByRole('dialog', { name: 'Go usage' })
  await expect(panel.getByText('5-hour window · 75%')).toBeVisible()
  await expect(panel.getByText('Weekly · 40%')).toBeVisible()
  await expect(panel.getByText('Monthly · 15%')).toBeVisible()
  await expect(panel.getByRole('definition')).toHaveText([/Resets in /, /Resets /, /Resets /])
  await expect(panel.getByText(/paid Zen credits/)).toHaveCount(0)
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`opencode-go-usage-${width}.png`), animations: 'disabled' })
  }

  workspace.setOpenCodeGoUsage(account.id, apiKey, {
    useBalance: true,
    rollingUsage: { status: 'ok', usagePercent: 25, resetInSec: 7200 },
    weeklyUsage: { status: 'ok', usagePercent: 60, resetInSec: 86400 },
    monthlyUsage: { status: 'rate-limited', usagePercent: 100, resetInSec: 604800 },
  })
  await workspace.api('/api/accounts/refresh', 'POST')
  await expect(panel.getByText('Monthly · 0%')).toBeVisible()
  await expect(panel.getByText(/Requests can use paid Zen credits/)).toBeVisible()
  const refreshed = (await workspace.api('/api/accounts')).accounts.find((a: { id: string }) => a.id === account.id)
  expect(refreshed.status).not.toBe('waiting')
  expect(refreshed.remainingPercent).toBe(0)
  expect(JSON.stringify(refreshed)).not.toContain(apiKey)
})

test('OpenCode explains an unavailable connection without claiming its quota is exhausted', async ({ page, workspace }) => {
  initializeRepository(workspace.projectPath)
  const accounts = (await workspace.api('/api/accounts')).accounts
  if (!accounts.some((account: { provider: string, enabled: boolean, state: string }) => account.provider === 'opencode' && account.enabled && account.state === 'ready')) {
    await workspace.api('/api/accounts', 'POST', {
      provider: 'opencode',
      name: 'Route diagnostic',
      modelProvider: 'opencode',
      apiKey: 'synthetic-browser-route-key',
    })
  }

  const chat = await workspace.api('/api/chats', 'POST', {})
  const model = 'openrouter/anthropic/claude-sonnet-4.6'
  try {
    await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', {
      id: crypto.randomUUID(),
      provider: 'opencode',
      model,
      text: 'Check the connection before starting.',
    })
    await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.accountWaitReason).toContain(`No enabled OpenCode account supports model ${model}`)
    await page.goto(workspacePath(`/chats/${chat.id}`))
    await signIn(page)
    for (const width of [1440, 390]) {
      await page.setViewportSize({ width, height: 900 })
      await expect(page.getByText(`No enabled OpenCode account supports model ${model}. Choose a model available through your connections or connect its model provider.`, { exact: true }).first()).toBeVisible()
      await expect(page.getByRole('status', { name: 'Coding agent quota reached' })).toHaveCount(0)
    }
  }
  finally {
    await workspace.api(`/api/chats/${chat.id}/stop`, 'POST')
  }
})
