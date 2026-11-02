import {
  signIn as authenticateWorkspace,
  expect,
  openAgentSection,
  saveAgent,
  test,
  workspacePath,
} from './fixtures'

test('contribution grants use resource ancestry, review, conflicts and explicit credential suppliers', async ({ page, workspace }, testInfo) => {
  test.setTimeout(120000)
  const prepare = async (path: string, body: unknown) => {
    const started = await workspace.api(path, 'POST', body)
    if (!started.operation)
      return started
    let operation: any
    await expect.poll(async () => {
      operation = await workspace.api(`/api/plugins/packages/operations/${started.operation}`)
      return operation.state
    }, { timeout: 30000 }).toBe('completed')
    const result = operation.result
    return result.kind === 'package' ? result.version : result.output.instance
  }

  const manifest = {
    id: 'grant-ui',
    name: 'Grant UI',
    version: '1',
    apiVersion: 2,
    capabilities: ['hooks'],
    agentAccess: { description: 'Read, post and receive messages.' },
    resources: [{ name: 'server', description: 'Servers' }, { name: 'channel', description: 'Channels', parentType: 'server' }, { name: 'thread', description: 'Threads', parentType: 'channel' }],
    tools: [{
      name: 'grant_ui_read',
      description: 'Read messages',
      inputSchema: { type: 'object', properties: { channel: { type: 'string' } } },
      resourceFields: [{ path: '/channel', resourceType: 'channel' }],
    }],
    commands: [{
      name: 'post',
      description: 'Post messages',
      output: 'json',
      arguments: [{ name: 'channel', positional: true, resourceType: 'channel' }],
    }],
    hooks: [{
      name: 'message',
      description: 'Receive messages',
      payloadSchema: { type: 'object', properties: { channel: { type: 'string' } } },
      resourceFields: [{ path: '/channel', resourceType: 'channel' }],
    }],
    instructions: 'Read carefully.',
    runCredentials: { git: [] },
  }
  const source = `return { resources = function(input)
    local kind = input.resourceType
    if input.op == "list" then
      if kind == "server" then return {items=cairn.array({{resourceType=kind,id="guild",name="Team"}})} end
      if kind == "thread" then return {items=cairn.array({{resourceType=kind,id="thread",name="Discussion",parent={resourceType="channel",id="support"}}})} end
      if input.query == "general" then return {items=cairn.array({{resourceType=kind,id="general",name="#general",parent={resourceType="server",id="guild"}}})} end
      local id = if input.page == 2 then "general" else "support"
      if input.query ~= "" and not string.find(id,input.query,1,true) then return {items=cairn.array({})} end
      return {items=cairn.array({{resourceType=kind,id=id,name="#"..id,parent={resourceType="server",id="guild"}}}),nextPage=if input.page == 1 and input.query == "" then 2 else nil}
    end
    return {resourceType=kind,id=input.id,name=if kind == "server" then "Team" elseif kind == "channel" then "#"..input.id else "Discussion",parent=if kind == "channel" then {resourceType="server",id="guild"} elseif kind == "thread" then {resourceType="channel",id="support"} else nil}
  end, tool=function() return {} end, command=function() return {} end }`
  const installed = await prepare('/api/plugins/packages', {
    manifest,
    manifestFormat: 'json',
    source,
    modules: {},
  })
  await prepare('/api/plugins/packages/grant-ui/approve', { pluginId: 'grant-ui', version: '1', digest: installed.digest })
  const instance = async (name: string) => prepare('/api/plugins/manage', {
    kind: 'save',
    pluginId: 'grant-ui',
    instanceId: null,
    data: {
      name,
      enabled: true,
      settings: {},
      agentId: '',
      projectId: null,
      accessMode: 'allowlist',
      userIds: [],
      people: [],
      maxMessagesPerHour: 100,
      maxPerPersonPerHour: 50,
    },
  })
  const first = (await instance('Support')).instance.id
  const second = (await instance('Automation')).instance.id
  await page.goto(workspacePath('/agents/new'))
  await authenticateWorkspace(page)
  await page.getByLabel('Name', { exact: true }).fill('New grant reviewer')
  await openAgentSection(page, 'Secrets & plugins')
  await page.getByRole('region', { name: 'Grant UI' }).getByRole('combobox', { name: 'Access to Support' }).selectOption('full')
  const newAgentSaveBar = page.getByRole('region', { name: 'Unsaved changes' })
  await newAgentSaveBar.getByRole('button', { name: 'Create agent', exact: true }).click()
  const newAgentReview = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(newAgentReview).toContainText('Only the installation owner can access a newly created agent')
  await expect(newAgentReview).toContainText('members do not have access by default')
  await expect(newAgentReview).toContainText('the no-code hook rule editor is not included in this change')
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-new-agent-review.png'), fullPage: true })
  await newAgentReview.getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page).toHaveURL(/\/agents\/[0-9a-f-]{36}$/)

  const agent = await workspace.api('/api/agents', 'POST', { name: 'Resource reviewer' })
  await workspace.api(`/api/agents/${agent.id}/plugin-grants`, 'PUT', {
    agentId: agent.id,
    revision: agent.revision,
    grants: [{ instanceId: first, access: { type: 'full' } }],
    runCredentialInstances: [first],
  })
  await page.addInitScript((agentId) => {
    Object.defineProperty(window, 'RTCPeerConnection', { configurable: true, value: undefined })

    type GrantReadTestWindow = Window & {
      __failNextAgentGrantRead: () => void
      __agentGrantReadFailed: boolean
      __failNextPluginCatalogRead: () => void
      __pluginCatalogReadFailed: boolean
    }

    const testWindow = window as unknown as GrantReadTestWindow
    let failNextRead = false
    let failNextCatalogRead = false
    const originalFetch = window.fetch.bind(window)
    testWindow.__agentGrantReadFailed = false
    testWindow.__pluginCatalogReadFailed = false
    testWindow.__failNextAgentGrantRead = () => {
      failNextRead = true
    }

    testWindow.__failNextPluginCatalogRead = () => {
      failNextCatalogRead = true
    }

    window.fetch = async (input, init) => {
      const requestUrl = input instanceof Request ? input.url : String(input)
      const method = init?.method ?? (input instanceof Request ? input.method : 'GET')
      const path = new URL(requestUrl, window.location.href).pathname
      if (failNextCatalogRead && method.toUpperCase() === 'GET' && path.endsWith('/api/plugins')) {
        failNextCatalogRead = false
        testWindow.__pluginCatalogReadFailed = true
        return new Response(JSON.stringify({ error: 'Plugins could not be loaded' }), {
          status: 503,
          headers: { 'Content-Type': 'application/json' },
        })
      }

      if (failNextRead && method.toUpperCase() === 'GET' && path.endsWith(`/api/agents/${agentId}/plugin-grants`)) {
        failNextRead = false
        testWindow.__agentGrantReadFailed = true
        return new Response(JSON.stringify({ error: 'Plugin grant state unavailable' }), {
          status: 503,
          headers: { 'Content-Type': 'application/json' },
        })
      }

      return originalFetch(input, init)
    }
  }, agent.id)
  await page.goto(workspacePath(`/agents/${agent.id}?section=plugins`))
  await authenticateWorkspace(page)
  const grantUi = page.getByRole('region', { name: 'Grant UI' })
  const support = grantUi.getByRole('combobox', { name: 'Access to Support' })
  const automation = grantUi.getByRole('combobox', { name: 'Access to Automation' })
  await support.selectOption('selected')
  for (const contribution of ['tool grant_ui_read', 'command post', 'hook message']) {
    await page.getByRole('checkbox', { name: contribution, exact: true }).check()
    const picker = page.getByRole('group', { name: 'Resource restrictions' }).last()
    await picker.getByLabel('Resource type').selectOption('channel')
    await expect(picker.getByText('Team › #support', { exact: true })).toBeVisible()
    await picker.getByRole('button', { name: 'Load more resources' }).click()
    await expect(picker.getByText('Team › #general', { exact: true })).toBeVisible()
    await picker.getByLabel('Search resources').fill('support')
    await picker.getByRole('checkbox', { name: 'Team › #support', exact: true }).check()
    await expect(picker.getByText('Only selected roots', { exact: false })).toBeVisible()
    if (contribution === 'tool grant_ui_read') {
      await picker.getByLabel('Search resources').fill('general')
      await picker.getByRole('checkbox', { name: 'Team › #general', exact: true }).check()
      await expect(picker.getByText('Team › #general · includes descendants', { exact: true })).toBeVisible()
    }

    await picker.getByLabel('Search resources').fill('general')
    await expect(picker.getByText('Team › #general', { exact: true })).toBeVisible()
    await expect(picker.getByText('Team › #support · includes descendants', { exact: true })).toBeVisible()
    await picker.getByLabel('Search resources').fill('support')
  }

  await automation.selectOption('full')
  await page.getByLabel('Supply run credentials from Automation', { exact: true }).check()
  await expect(page.getByText('Current supplier: Automation.', { exact: false }).first()).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-editor.png'), fullPage: true })
  const hookContribution = page.getByRole('checkbox', { name: 'hook message', exact: true })
  await hookContribution.scrollIntoViewIfNeeded()
  await expect(hookContribution).toBeInViewport()
  const hookResources = hookContribution.locator('xpath=../..').getByRole('group', { name: 'Resource restrictions' })
  await expect(hookResources.getByText('Team › #support', { exact: true })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-hook.png') })
  const resourceSearch = page.getByLabel('Search resources').first()
  await resourceSearch.evaluate(element => element.scrollIntoView({ block: 'center', inline: 'nearest' }))
  await expect(resourceSearch).toBeInViewport()
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-resource-picker.png') })
  const original = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(original.grants).toEqual([{ instanceId: first, access: { type: 'full' } }])
  await page.setViewportSize({ width: 390, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('channel support and descendants')
  await expect(review).toContainText('Run credential suppliers: Grant UI “Automation”')
  await expect(review).toContainText('Accounts with access to this agent:')
  await expect(review).toContainText('the no-code hook rule editor is not included in this change')
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-review.png'), fullPage: true })
  await review.getByRole('button', { name: 'Back to editing' }).click()
  // This instance is created after the page's initial catalog read, so only a fresh catalog can
  // identify it as another instance of the same package during the conflict rebase.
  const remote = (await instance('Review')).instance.id
  await workspace.api(`/api/agents/${agent.id}/plugin-grants`, 'PUT', {
    agentId: agent.id,
    revision: original.revision,
    grants: [
      { instanceId: first, access: { type: 'full' } },
      { instanceId: remote, access: { type: 'full' } },
    ],
    runCredentialInstances: [remote],
  })
  const concurrentGrant = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(concurrentGrant.grants).toHaveLength(2)
  expect(concurrentGrant.grants).toContainEqual(expect.objectContaining({ instanceId: remote }))
  // A concurrent profile edit shares the grant revision. Reload preserves this grant draft.
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { description: 'Changed elsewhere' })
  const afterProfileEdit = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(afterProfileEdit.grants).toContainEqual(concurrentGrant.grants[0])
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  await page.evaluate(() => {
    const testWindow = window as unknown as Window & { __failNextPluginCatalogRead: () => void }
    testWindow.__failNextPluginCatalogRead()
  })
  // Keep both the conflict and local supplier draft retryable while the current package mapping
  // is unavailable.
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await expect.poll(() => page.evaluate(() => {
    const testWindow = window as unknown as Window & { __pluginCatalogReadFailed: boolean }
    return testWindow.__pluginCatalogReadFailed
  })).toBe(true)
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toBeEnabled()
  await expect(page.getByRole('list', { name: 'Held plugin access' })).toContainText('Granted instance')
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toHaveCount(0)
  await expect(page.getByRole('combobox', { name: 'Access to Review' })).toHaveValue('full')
  await expect(page.getByLabel('Supply run credentials from Automation', { exact: true })).toBeChecked()
  await expect(page.getByLabel('Supply run credentials from Review', { exact: true })).not.toBeChecked()
  await expect(page.getByRole('group', { name: 'Resource restrictions' }).first()).toContainText('Team › #general · includes descendants')
  await saveAgent(page, true)
  const saved = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  const savedSupport = saved.grants.find((grant: any) => grant.instanceId === first)
  expect(savedSupport.access.contributions).toEqual(expect.arrayContaining([
    { kind: 'tool', name: 'grant_ui_read', resources: expect.arrayContaining([{ resourceType: 'channel', id: 'support' }, { resourceType: 'channel', id: 'general' }]) },
    { kind: 'command', name: 'post', resources: [{ resourceType: 'channel', id: 'support' }] },
    { kind: 'hook', name: 'message', resources: [{ resourceType: 'channel', id: 'support' }] },
  ]))
  expect(saved.grants).toContainEqual({ instanceId: second, access: { type: 'full' } })
  expect(saved.runCredentialInstances).toEqual([second])
  expect(saved.grants).toContainEqual({
    instanceId: remote,
    access: { type: 'full' },
  })
  const summaryRefreshTrigger = await workspace.api('/api/agents', 'POST', { name: 'Summary refresh trigger' })
  await page.reload()
  await expect(support).toHaveValue('selected')
  await automation.selectOption('none')
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const supplierReview = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(supplierReview).toContainText('Active runs keep credentials already loaded until they finish.')
  await supplierReview.getByRole('button', { name: 'Save', exact: true }).click()
  const afterRemovingSupplier = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(afterRemovingSupplier.runCredentialInstances).toEqual([])
  expect(afterRemovingSupplier.grants.map((grant: any) => grant.instanceId)).toEqual(
    expect.arrayContaining([first, remote]),
  )
  await page.goto(workspacePath('/agents'))
  await expect(page.getByText('Selected plugin access', { exact: false })).toBeVisible()

  const initialGrantRead = page.waitForResponse(response =>
    response.url().endsWith(`/api/agents/${agent.id}/plugin-grants`)
    && response.request().method() === 'GET'
    && response.ok(),
  )
  await page.goto(workspacePath(`/agents/${agent.id}?section=identity`))
  await authenticateWorkspace(page)
  await initialGrantRead
  await page.getByLabel('Name', { exact: true }).fill('Local name after retry')
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { description: 'Remote description survives retry' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toBeVisible()
  await page.evaluate(() => {
    const testWindow = window as unknown as Window & { __failNextAgentGrantRead: () => void }
    testWindow.__failNextAgentGrantRead()
  })
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await expect.poll(() => page.evaluate(() => {
    const testWindow = window as unknown as Window & { __agentGrantReadFailed: boolean }
    return testWindow.__agentGrantReadFailed
  })).toBe(true)
  await expect(page.getByRole('alert')).toContainText('changed elsewhere')
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toHaveCount(0)
  await expect(page.getByLabel('Name', { exact: true })).toHaveValue('Local name after retry')
  await expect(page.getByLabel('Description', { exact: true })).toHaveValue('Remote description survives retry')
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Local name after retry', exact: true })).toBeVisible()
  const savedAgent = await workspace.api(`/api/agents/${agent.id}`)
  expect(savedAgent).toMatchObject({ name: 'Local name after retry', description: 'Remote description survives retry' })

  await page.getByLabel('Name', { exact: true }).fill('Local name during grant reload')
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { description: 'Second remote description' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toBeVisible()
  await page.getByRole('link', { name: 'All settings', exact: true }).click()
  await page.getByRole('link', { name: /Secrets & plugins/ }).click()
  let releaseGrantRead!: () => void
  let grantReadStarted!: () => void
  const grantReadGate = new Promise<void>((resolve) => {
    releaseGrantRead = resolve
  })
  const grantReadRequest = new Promise<void>((resolve) => {
    grantReadStarted = resolve
  })
  const grantReadPattern = `**/api/agents/${agent.id}/plugin-grants`
  await page.route(grantReadPattern, async (route) => {
    if (route.request().method() !== 'GET') {
      await route.continue()
      return
    }

    const response = await route.fetch()
    grantReadStarted()
    await grantReadGate
    await route.fulfill({ response })
  })
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await grantReadRequest
  await expect(page.getByRole('combobox', { name: 'Access to Support' })).toBeDisabled()
  releaseGrantRead()
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toHaveCount(0)
  await page.unroute(grantReadPattern)

  await workspace.restart()
  const persistedGrants = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(persistedGrants.grants.map((grant: any) => grant.instanceId)).toEqual(
    expect.arrayContaining([first, remote]),
  )
  await page.route('**/api/plugins', route => route.fulfill({
    status: 503,
    contentType: 'application/json',
    body: JSON.stringify({ error: 'Plugins could not be loaded' }),
  }))
  await page.goto(workspacePath(`/agents/${agent.id}?section=plugins`))
  await authenticateWorkspace(page)
  await expect(page.getByRole('list', { name: 'Held plugin access' }))
    .toContainText(`Granted instance ${first.slice(0, 8)}…`)
  await page.unroute('**/api/plugins')

  await page.route('**/api/plugins', async (route) => {
    const response = await route.fetch()
    const catalog = await response.json()
    const grantUi = catalog.packages.find((item: any) => item.id === 'grant-ui')
    grantUi.manifest.tools = grantUi.manifest.tools.filter((item: any) => item.name !== 'grant_ui_read')
    grantUi.manifest.commands[0].arguments = []
    await route.fulfill({ response, json: catalog })
  })
  await page.goto(workspacePath(`/agents/${agent.id}?section=plugins`))
  await authenticateWorkspace(page)
  const staleGrant = page.getByRole('button', { name: 'Remove stale grant tool grant_ui_read', exact: true })
  const staleGrantAlert = page.getByRole('alert').filter({ hasText: 'Some saved grants refer to' })
  await expect(staleGrantAlert).toContainText('contributions this plugin no longer provides')
  await expect(staleGrant).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-stale.png'), fullPage: true })
  await staleGrant.click()
  await expect(staleGrant).toHaveCount(0)
  await expect(page.getByRole('checkbox', { name: 'command post', exact: true })).toBeChecked()
  const staleRoot = page.getByRole('button', {
    name: 'Remove stale resource restriction command post channel support',
    exact: true,
  })
  const staleResourceAlert = page.getByRole('alert').filter({ hasText: 'Saved resource roots include' })
  await expect(staleResourceAlert).toContainText('Requests fail closed until corrected')
  await expect(staleRoot).toBeVisible()
  await expect(page.getByText('No resource restriction.', { exact: true })).toHaveCount(0)
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-stale-resource.png'), fullPage: true })
  await staleRoot.click()
  await expect(staleRoot).toHaveCount(0)
  await expect(page.getByText('No resource restriction.', { exact: true })).toBeVisible()
  await saveAgent(page, true)
  const afterStaleRemoval = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  const remainingSupport = afterStaleRemoval.grants.find((grant: any) => grant.instanceId === first)
  expect(remainingSupport.access.contributions).toEqual(expect.arrayContaining([
    { kind: 'command', name: 'post', resources: [] },
    { kind: 'hook', name: 'message', resources: [{ resourceType: 'channel', id: 'support' }] },
  ]))
  expect(remainingSupport.access.contributions).not.toContainEqual(expect.objectContaining({ kind: 'tool', name: 'grant_ui_read' }))

  await page.goto(workspacePath('/agents'))
  const agentCard = page.locator('article').filter({ has: page.locator(`a[href$="${agent.id}"]`) })
  await expect(agentCard).toContainText('Selected plugin access')
  await workspace.restart()
  await page.goto(workspacePath(`/agents/${agent.id}?section=plugins`))
  const supportAccess = page.getByRole('switch', { name: 'Access to Grant UI Support', exact: true })
  await expect(supportAccess).toBeChecked()
  await supportAccess.uncheck()
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const revocationReview = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(revocationReview).toContainText('Removed: Grant UI “Support” access. Plugin permission checks update immediately, including for running conversations. Credentials already loaded into active runs remain available until those runs finish.')
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-revocation-review.png'), fullPage: true })
  expect((await workspace.api(`/api/agents/${agent.id}/plugin-grants`)).grants).toContainEqual(expect.objectContaining({ instanceId: first }))
  await revocationReview.getByRole('button', { name: 'Save', exact: true }).click()
  await expect.poll(async () => (await workspace.api(`/api/agents/${agent.id}/plugin-grants`)).grants.some((grant: any) => grant.instanceId === first)).toBe(false)
  await page.goto(workspacePath('/agents'))
  await expect(agentCard).not.toContainText('Selected plugin access')
  const latestAgent = await workspace.api(`/api/agents/${agent.id}`)
  await workspace.api(`/api/agents/${agent.id}/plugin-grants`, 'PUT', {
    agentId: agent.id,
    revision: latestAgent.revision,
    grants: [],
    runCredentialInstances: [],
  })
  await page.getByRole('button', { name: `Delete ${summaryRefreshTrigger.name}` }).click()
  await page.getByRole('dialog', { name: 'Remove this item?' }).getByRole('button', { name: 'Remove' }).click()
  await expect(agentCard).not.toContainText('Selected plugin access')
})

test('agent overview shows plugin access as unavailable when the grant read fails', async ({ page, workspace }) => {
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Grant summary reader' })
  await workspace.restart()
  await page.route(`**/api/agents/${agent.id}/plugin-grants`, route => route.fulfill({
    status: 503,
    contentType: 'application/json',
    body: JSON.stringify({ error: 'Plugin grant state unavailable' }),
  }))
  await page.goto(workspacePath(`/agents/${agent.id}`))
  await authenticateWorkspace(page)

  const plugins = page.getByRole('button', { name: /Secrets & plugins/ })
  await expect(plugins).toContainText('Plugin access could not be loaded')
  await expect(plugins).toContainText('Unavailable')
  await expect(plugins).not.toContainText('No plugin access by default')
})
