import type { CodingAgentAccounts } from '../../packages/contracts/generated/typescript/CodingAgentAccounts'
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

const termsWarning = 'Google’s Antigravity terms prohibit signing in with a consumer Google account from third-party applications such as Cairn; Google may suspend the account. Prefer a Gemini API key or Google Cloud credentials.'

let existingAccountIds: Set<string>

test.beforeEach(async ({ workspace }) => {
  existingAccountIds = new Set((await workspace.api('/api/accounts')).accounts.map((account: { id: string }) => account.id))
})

// Each journey connects its own accounts; later journeys in this worker start without them.
test.afterEach(async ({ workspace }) => {
  let removableAccountIds: string[] = []
  await expect.poll(async () => {
    const { accounts, signIn }: CodingAgentAccounts = await workspace.api('/api/accounts')
    const createdAccounts = accounts.filter(account => !existingAccountIds.has(account.id))
    removableAccountIds = createdAccounts.map(account => account.id)

    // Credentials can be ready while sign-in still owns the account for cleanup.
    return {
      activeRunIds: createdAccounts.flatMap(account => account.activeRunIds),
      signingIn: signIn?.state === 'pending' && removableAccountIds.includes(signIn.accountId),
    }
  }, { timeout: 15000 }).toEqual({ activeRunIds: [], signingIn: false })

  for (const id of removableAccountIds)
    await workspace.api(`/api/accounts/${id}`, 'DELETE')
})

test('Antigravity connects a Gemini API key, selects a model and runs a conversation', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  initializeRepository(workspace.projectPath)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(workspacePath('/connections'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Add an account' })
  await dialog.getByRole('radio', { name: /Antigravity/ }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('Gemini key')
  const connection = dialog.getByRole('radiogroup', { name: 'Connection' })
  await connection.getByRole('radio', { name: /^Gemini API key/ }).click()
  await expect(connection.getByRole('radio', { name: /^Gemini API key/ })).toHaveAttribute('aria-checked', 'true')
  await expect(dialog.getByRole('button', { name: 'Save API key' })).toBeDisabled()
  await dialog.getByLabel('API key', { exact: true }).fill('synthetic-browser-gemini-key')
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`antigravity-connect-${width}.png`), animations: 'disabled' })
  }

  await dialog.getByRole('button', { name: 'Save API key' }).click()
  await expect(dialog).toHaveCount(0)
  const accounts = (await workspace.api('/api/accounts')).accounts
  const account = accounts.find((a: { provider: string }) => a.provider === 'antigravity')
  expect(account).toMatchObject({
    name: 'Gemini key',
    modelProvider: 'gemini',
    modelProviderName: 'Gemini API',
    state: 'ready',
  })
  expect(JSON.stringify(accounts)).not.toContain('synthetic-browser-gemini-key')
  await expect(page.getByRole('region', { name: 'Antigravity accounts' })).toContainText('Gemini key')

  await page.goto(workspacePath('/agents'))
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Antigravity workspace helper')
  await grantFullAccess(page)
  await openAgentSection(page, 'Coding agent & model')
  await page.getByRole('button', { name: /^Agent, model, reasoning and speed/ }).click()
  const menu = page.getByRole('dialog', { name: 'Agent & model' })
  await menu.getByRole('radio', { name: 'Antigravity', exact: true }).click()
  // One entry per model family; its levels are the reasoning choices.
  await menu.getByRole('radio', { name: 'Gemini 4 Neon', exact: true }).click()
  await expect(menu.getByText('Via Gemini API').first()).toBeVisible()
  const reasoning = menu.getByRole('radiogroup', { name: 'Reasoning' })
  await expect(reasoning.getByRole('radio')).toHaveText(['Model default · High', 'Low', 'High', 'Extra high'])
  await reasoning.getByRole('radio', { name: 'High', exact: true }).click()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`antigravity-models-${width}.png`), animations: 'disabled' })
  }

  await menu.getByRole('button', { name: 'Close', exact: true }).click()
  await createAgent(page)
  const agent = (await workspace.api('/api/agents')).find((a: { name: string }) => a.name === 'Antigravity workspace helper')!
  expect(agent).toMatchObject({ provider: 'antigravity', model: 'gemini-4-neon', reasoning: 'high' })
  await page.getByRole('link', { name: 'Chat with this agent' }).click()
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Prepare a compact workspace status panel.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect(page).toHaveURL(/\/chats\/[a-z]+-[a-z]+-[a-f0-9]{8}$/)
  const slug = new URL(page.url()).pathname.split('/').at(-1)!
  await expect.poll(async () => {
    const { run } = await workspace.api(`/api/chats/${slug}`)
    if (run?.status === 'failed')
      throw new Error(run.error)
    return run?.status
  }, { timeout: 30000 }).toBe('succeeded')
  await expect(page.getByText(/I prepared a compact status panel\. You asked: "Prepare a compact workspace status panel\."/).last()).toBeVisible()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await expectSingleScroll(page)
    await page.screenshot({ path: testInfo.outputPath(`antigravity-conversation-${width}.png`), animations: 'disabled' })
  }

  // The next message resumes the same native conversation.
  const previous = await workspace.api(`/api/chats/${slug}`)
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Now add the project status.')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run?.summary, { timeout: 30000 }).toContain('Now add the project status.')
  await expect.poll(async () => (await workspace.api(`/api/chats/${slug}`)).run.status, { timeout: 30000 }).toBe('succeeded')
  expect((await workspace.api(`/api/chats/${slug}`)).run.sessionId).toBe(previous.run.sessionId)
  expect(errors).toEqual([])
})

test('Antigravity shows Google’s terms and requires confirmation before a Google account sign-in', async ({ page, workspace }, testInfo) => {
  test.setTimeout(90000)
  const errors: string[] = []
  page.on('pageerror', error => errors.push(error.message))
  await page.goto(workspacePath('/connections'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Add an account' })
  await dialog.getByRole('radio', { name: /Antigravity/ }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('Personal Google')
  await dialog.getByRole('radiogroup', { name: 'Connection' }).getByRole('radio', { name: /^Google account/ }).click()
  const warning = dialog.getByRole('alert').filter({ hasText: termsWarning })
  await expect(warning).toBeVisible()
  const start = dialog.getByRole('button', { name: /Continue to sign-in/ })
  await expect(start).toBeDisabled()
  for (const [width, colorScheme] of [[1440, 'light'], [390, 'dark']] as const) {
    await page.setViewportSize({ width, height: 900 })
    await page.emulateMedia({ colorScheme })
    await page.screenshot({ path: testInfo.outputPath(`antigravity-terms-${width}.png`), animations: 'disabled' })
  }

  // Nothing starts before the confirmation.
  expect((await workspace.api('/api/accounts')).signIn ?? null).toBeNull()
  await warning.getByRole('checkbox').check()
  await expect(start).toBeEnabled()
  await start.click()

  const flow = page.getByRole('dialog', { name: 'Connect your Google account' })
  const link = flow.getByRole('link', { name: /Open Google sign-in/ })
  await expect(link).toHaveAttribute('href', /^https:\/\/accounts\.google\.com\/o\/oauth2\/auth\?/)
  await flow.getByLabel('Google authorization code').fill('4/synthetic-browser-code')
  await flow.getByRole('button', { name: 'Finish sign-in', exact: true }).click()
  await expect.poll(async () => {
    const { accounts } = await workspace.api('/api/accounts')
    return accounts.find((a: { name: string }) => a.name === 'Personal Google')
  }, { timeout: 30000 }).toMatchObject({
    provider: 'antigravity',
    email: 'fixture.user@gmail.com',
    modelProvider: 'google',
    modelProviderName: 'Google account',
    state: 'ready',
  })
  const accountRow = page.getByRole('region', { name: 'Antigravity accounts' }).getByRole('button', { name: /^Personal Google,/ })
  await expect(accountRow).toBeVisible()
  await expect(flow).toHaveCount(0)
  await accountRow.click()
  const details = page.getByRole('dialog', { name: 'Personal Google' })
  await expect(details.getByText('Use Check usage now in Connections to check this Google account’s quota.')).toBeVisible()
  await details.getByRole('button', { name: 'Close dialog', exact: true }).click()
  await page.getByRole('button', { name: 'Check usage now' }).click()
  await expect(accountRow).toContainText('See quotas in account details', { timeout: 20000 })
  await accountRow.click()
  await expect(details.getByText('Weekly · Gemini Models')).toBeVisible()
  await expect(details.getByRole('progressbar', { name: 'Personal Google Weekly · Gemini Models remaining' })).toHaveAttribute('aria-valuenow', '75')
  await expect(details.getByText('5-hour window · Claude and GPT models')).toBeVisible()
  await expect(details.getByText(/\bResets\s+\S/)).toHaveCount(2)
  expect(errors).toEqual([])
})

test('Antigravity composes Google Cloud credentials with their project and region', async ({ page, workspace }) => {
  test.setTimeout(90000)
  await page.goto(workspacePath('/connections'))
  await signIn(page)
  await page.getByRole('button', { name: 'Add account', exact: true }).click()
  const dialog = page.getByRole('dialog', { name: 'Add an account' })
  await dialog.getByRole('radio', { name: /Antigravity/ }).click()
  await dialog.getByLabel('Name', { exact: true }).fill('Vertex project')
  await dialog.getByRole('radiogroup', { name: 'Connection' }).getByRole('radio', { name: /^Google Cloud credentials/ }).click()
  const save = dialog.getByRole('button', { name: 'Save credentials' })
  await expect(save).toBeDisabled()
  const serviceAccount = {
    type: 'service_account',
    project_id: 'synthetic-browser-project',
    private_key: '-----BEGIN PRIVATE KEY-----\nsynthetic-browser-private-key\n-----END PRIVATE KEY-----\n',
    client_email: 'agent@synthetic-browser-project.iam.gserviceaccount.com',
  }
  await dialog.getByLabel('Credentials JSON').fill(JSON.stringify(serviceAccount))
  // The project defaults to the one the file names.
  await expect(dialog.getByLabel('Project', { exact: true })).toHaveAttribute('placeholder', 'synthetic-browser-project')
  await dialog.getByLabel('Region', { exact: true }).fill('us-central1')
  await save.click()
  await expect(dialog).toHaveCount(0)
  const accounts = (await workspace.api('/api/accounts')).accounts
  expect(accounts.find((a: { name: string }) => a.name === 'Vertex project')).toMatchObject({
    provider: 'antigravity',
    modelProvider: 'google-cloud',
    modelProviderName: 'Google Cloud',
    state: 'ready',
  })
  expect(JSON.stringify(accounts)).not.toContain('synthetic-browser-private-key')
  // The saved account's catalog includes the Google Cloud models.
  const catalog = await workspace.api('/api/antigravity/models')
  expect(catalog.models.find((m: { model: string }) => m.model === 'claude-sonnet-4-6-thinking')).toMatchObject({ description: 'Via Google Cloud' })
})
