import type { Workspace } from './fixtures'
import {
  createAgent,
  expect,
  expectSingleScroll,
  expectUncovered,
  grantFullAccess,
  openAgentSection,
  signIn,
  test,
  workspacePath,
} from './fixtures'

const read = (workspace: Workspace, provider = 'codex', agentId: string | null = null) => workspace.api('/api/settings/coding-agents/read', 'POST', { provider, agentId })
test.beforeEach(async ({ page, workspace }) => {
  await page.goto(workspace.url)
  await signIn(page)
})

test('new agents inherit installation preferences saved after the catalog was loaded', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await save(workspace, '{}', 'opencode')
  await workspace.api('/api/accounts', 'POST', {
    provider: 'opencode',
    name: 'Configuration catalog',
    modelProvider: 'opencode',
    apiKey: 'synthetic-browser-zen-key',
  })
  const existing = await workspace.api('/api/agents', 'POST', {
    name: 'Catalog primer',
    provider: 'opencode',
    model: 'opencode/ling-3.1-flash-free',
    reasoning: '',
  })
  await page.goto(workspacePath(`/agents/${existing.id}?section=model`))
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await picker.click()
  await expect(page.getByRole('dialog', { name: 'Agent & model' }).getByRole('radio', { name: 'Ling 3.1 Flash Free', exact: true })).toBeVisible()
  await page.keyboard.press('Escape')
  await page.getByRole('link', { name: 'Back to agents', exact: true }).click()
  await page.getByRole('navigation', { name: 'Atelier sections' }).getByRole('link', { name: 'Settings', exact: true }).click()
  await page.getByRole('link', { name: 'Installation', exact: true }).click()
  await page.getByRole('link', { name: 'OpenCode opencode.jsonc', exact: true }).click()
  await page.getByLabel('Model', { exact: true }).fill('opencode/configured-selection')
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
  await page.getByRole('link', { name: 'Back to installation settings', exact: true }).click()
  await page.getByRole('navigation', { name: 'Atelier sections' }).getByRole('link', { name: 'Agents', exact: true }).click()
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Inherited catalog selection')
  await grantFullAccess(page)
  await openAgentSection(page, 'Coding agent & model')
  await picker.click()
  await page.getByRole('dialog', { name: 'Agent & model' }).getByRole('radio', { name: 'OpenCode', exact: true }).click()
  await page.keyboard.press('Escape')
  await expect(picker).toHaveAccessibleName(/opencode\/configured-selection/)
  await createAgent(page)
  const created = (await workspace.api('/api/agents')).find((agent: { name: string }) => agent.name === 'Inherited catalog selection')
  expect(created.model).toBe('')
  expect(created.codingAgentDefaults.opencode.model).toBe('opencode/configured-selection')
})

async function save(workspace: Workspace, document: string, provider = 'codex', agentId: string | null = null) {
  const current = await read(workspace, provider, agentId)
  return workspace.api('/api/settings/coding-agents', 'PUT', {
    provider,
    agentId,
    document,
    revision: current.revision,
    changes: [],
    reset: false,
  })
}

test('new agent reasoning inherits the installation without an implicit personalization', async ({ page, workspace }) => {
  test.setTimeout(60000)
  await save(workspace, 'model = "fixture-deep"\nmodel_reasoning_effort = "ultra"\n')
  await page.goto(workspacePath('/agents'))
  await page.getByRole('link', { name: 'New agent', exact: true }).click()
  await page.getByLabel('Name', { exact: true }).fill('Inherited reasoning')
  await grantFullAccess(page)
  await openAgentSection(page, 'Coding agent & model')
  const picker = page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })
  await expect(picker).toHaveAccessibleName(/Ultra reasoning/)
  await picker.click()
  await page.getByRole('dialog', { name: 'Agent & model' }).getByRole('radio', { name: 'Deep thinker', exact: true }).click()
  await page.keyboard.press('Escape')
  await expect(picker).toHaveAccessibleName(/Ultra reasoning/)
  const id = await createAgent(page)
  const created = (await workspace.api('/api/agents')).find((agent: { id: string }) => agent.id === id)
  expect(created.reasoning).toBe('')
  expect(created.codingAgentDefaults.codex.reasoning).toBe('ultra')
  const chat = await workspace.api('/api/chats', 'POST', { agentId: id })
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'Use the inherited effort' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.snapshot.agent.reasoning).toBe('ultra')
})

test('native form and text share comments and advanced options across save and reload', async ({ page, workspace }) => {
  const viewport = page.viewportSize()
  await save(workspace, '# Context preference\nmodel_context_window = 200000 # keep this note\nproject_doc_max_bytes = 32768\n')
  await page.goto(workspacePath('/settings/coding-agents/codex'))
  await expect(page.getByRole('heading', { name: 'Codex configuration', exact: true })).toBeVisible()
  const context = page.getByLabel('Context window', { exact: true })
  await expect(context).toHaveValue('200000')
  await context.fill('1000000')
  await expectUncovered(context)
  await expect(page.getByRole('button', { name: 'Save configuration', exact: true })).toBeEnabled()
  await test.info().attach('native-form', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' })
  await page.setViewportSize({ width: 360, height: 640 })
  await page.keyboard.press('Tab')
  await context.fill('1000000')
  await expectUncovered(context)
  if (viewport)
    await page.setViewportSize(viewport)
  await page.getByRole('button', { name: 'config.toml', exact: true }).click()
  const raw = page.getByRole('textbox', { name: 'Native configuration', exact: true })
  await expect(raw).toHaveValue(/model_context_window = 1000000/)
  await expect(raw).toHaveValue(/# Context preference/)
  await expect(raw).toHaveValue(/# keep this note/)
  await expect(raw).toHaveValue(/project_doc_max_bytes = 32768/)
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
  await page.reload()
  await page.getByRole('button', { name: 'config.toml', exact: true }).click()
  await expect(raw).toHaveValue(/model_context_window = 1000000/)
  await expect(raw).toHaveValue(/# keep this note/)
  await expectUncovered(page.getByRole('button', { name: 'Save configuration', exact: true }))
  await expectSingleScroll(page)
  await test.info().attach('native-editor', { body: await page.screenshot({ fullPage: true }), contentType: 'image/png' })
})

test('agent customizations reset to live installation values without pinning unrelated settings', async ({ page, workspace }) => {
  await save(workspace, 'model_context_window = 200000\nmodel_auto_compact_token_limit = 180000\n')
  const agent = await workspace.api('/api/agents', 'POST', {
    name: 'Configuration inheritance',
    provider: 'codex',
    model: '',
    reasoning: '',
  })
  await page.goto(workspacePath(`/agents/${agent.id}/configuration/codex`))
  const context = page.getByLabel('Context window', { exact: true })
  await expect(context).toHaveValue('200000')
  await context.fill('1000000')
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
  await save(workspace, 'model_context_window = 1000000\nmodel_auto_compact_token_limit = 350000\n')
  await page.reload()
  await expect(context).toHaveValue('1000000')
  await expect(page.getByLabel('Automatic compaction threshold', { exact: true })).toHaveValue('350000')
  await page.getByRole('button', { name: 'Use installation value', exact: true }).click()
  await expect(context).toHaveValue('1000000')
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
  await save(workspace, 'model_context_window = 500000\n')
  await page.reload()
  await expect(context).toHaveValue('500000')
})

test('invalid fields remain editable and unsaved text survives a cancelled navigation', async ({ page, workspace }) => {
  await save(workspace, '')
  await page.goto(workspacePath('/settings/coding-agents/codex'))
  const context = page.getByLabel('Context window', { exact: true })
  await context.fill('0')
  await expect(page.getByRole('alert')).toContainText('positive whole number')
  await expect(context).toBeEnabled()
  await context.fill('1000000')
  await expect(page.getByRole('button', { name: 'Save configuration', exact: true })).toBeEnabled()
  await page.getByRole('link', { name: 'Back to installation settings', exact: true }).click()
  await page.getByRole('button', { name: 'Keep editing', exact: true }).click()
  await expect(context).toHaveValue('1000000')
  await page.getByRole('button', { name: 'config.toml', exact: true }).click()
  const raw = page.getByRole('textbox', { name: 'Native configuration', exact: true })
  await raw.fill('model_context_window = [')
  await expect(page.getByRole('alert')).toContainText('Invalid TOML')
  await expect(page.getByRole('button', { name: 'Save configuration', exact: true })).toBeDisabled()
  await raw.fill('model_context_window = 900000\n')
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
})

test('reset all replaces invalid form drafts and can save the engine defaults', async ({ page, workspace }) => {
  await save(workspace, 'model_context_window = 200000\n')
  await page.goto(workspacePath('/settings/coding-agents/codex'))
  const context = page.getByLabel('Context window', { exact: true })
  const compaction = page.getByLabel('Automatic compaction threshold', { exact: true })
  await compaction.fill('800000')
  await context.fill('0')
  await expect(page.getByRole('alert')).toContainText('positive whole number')
  await page.getByRole('button', { name: 'Restore engine defaults', exact: true }).click()
  await expect(context).toHaveValue('')
  await expect(compaction).toHaveValue('')
  await expect(page.getByRole('button', { name: 'Save configuration', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
  await expect.poll(async () => (await read(workspace)).document.trim()).toBe('')
})

test('Claude and OpenCode use their native JSON editors and reject unsupported options', async ({ page, workspace }) => {
  await save(workspace, '{"language":"French","spinnerTipsEnabled":false}\n', 'claude')
  await page.goto(workspacePath('/settings/coding-agents/claude'))
  await expect(page.getByLabel('Response language', { exact: true })).toHaveValue('French')
  await page.getByLabel('Response language', { exact: true }).fill('English')
  await page.getByRole('button', { name: 'Save configuration', exact: true }).click()
  await expect(page.getByText('All changes saved', { exact: true })).toBeVisible()
  await save(workspace, '{\n // Workspace preference\n "snapshot": false,\n "formatter": {},\n}\n', 'opencode')
  await page.goto(workspacePath('/settings/coding-agents/opencode'))
  await page.getByRole('button', { name: 'opencode.jsonc', exact: true }).click()
  const raw = page.getByRole('textbox', { name: 'Native configuration', exact: true })
  await expect(raw).toHaveValue(/Workspace preference/)
  await raw.fill('{"compaction":{"auto":true}}')
  await expect(page.getByRole('alert')).toContainText('not supported')
  await expect(page.getByRole('button', { name: 'Save configuration', exact: true })).toBeDisabled()
})

test('an existing conversation displays and launches the changed defaults on its next message', async ({ page, workspace }) => {
  const main = (await workspace.api('/api/agents')).find((agent: { name: string }) => agent.name === 'Main agent')
  const agent = await workspace.api('/api/agents', 'POST', {
    name: 'Live native defaults',
    model: '',
    reasoning: '',
    access: main.access,
  })
  await save(workspace, 'model = "fixture-fast"\nmodel_context_window = 200000\n')
  const chat = await workspace.api('/api/chats', 'POST', { agentId: agent.id })
  await workspace.api(`/api/chats/${chat.id}/messages`, 'POST', { id: crypto.randomUUID(), text: 'First native preferences' })
  await expect.poll(async () => (await workspace.api(`/api/chats/${chat.id}`)).run?.status).toBe('succeeded')
  await save(workspace, 'model = "fixture-deep"\nmodel_context_window = 1000000\n')
  await page.goto(workspacePath(`/chats/${chat.id}`))
  await expect(page.getByRole('button', { name: /^Agent, model, reasoning and speed/ })).toHaveAccessibleName(/Deep thinker/)
  await page.getByRole('textbox', { name: 'Message', exact: true }).fill('Use the updated preferences')
  await page.getByRole('button', { name: 'Send', exact: true }).click()
  await expect.poll(async () => {
    const current = await workspace.api(`/api/chats/${chat.id}`)
    return { model: current.run?.snapshot.agent.model, document: current.run?.snapshot.nativeConfiguration?.document }
  }).toEqual({ model: 'fixture-deep', document: expect.stringContaining('model_context_window = 1000000') })
})
