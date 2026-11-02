import {
  signIn as authenticateWorkspace,
  expect,
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
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-editor.png'), fullPage: true })
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.getByRole('group', { name: 'Resource restrictions' }).first().scrollIntoViewIfNeeded()
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-resource-picker.png'), fullPage: true })
  const original = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(original.grants).toEqual([{ instanceId: first, access: { type: 'full' } }])
  await page.setViewportSize({ width: 320, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('channel support and descendants')
  await expect(review).toContainText('Run credential suppliers: Grant UI “Automation”')
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
  await page.reload()
  await expect(support).toHaveValue('selected')
  await automation.selectOption('none')
  await saveAgent(page, true)
  expect((await workspace.api(`/api/agents/${agent.id}/plugin-grants`)).runCredentialInstances).toEqual([])
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

  await page.goto(workspacePath(`/agents/${agent.id}?section=plugins`))
  await authenticateWorkspace(page)
  const supportSwitch = page.getByRole('switch', { name: 'Access to Grant UI Support' })
  await supportSwitch.uncheck()
  await expect(page.getByRole('combobox', { name: 'Access to Support' })).toHaveValue('none')
  await expect(page.getByText('Only checked contributions are granted.', { exact: false })).toHaveCount(0)
})

test('agent overview shows plugin access as unavailable when the grant read fails', async ({ page, workspace }) => {
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Grant summary reader' })
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
