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
  const remote = (await instance('Review')).instance.id
  const agent = await workspace.api('/api/agents', 'POST', { name: 'Resource reviewer' })
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
    await picker.getByLabel('Search resources').fill('general')
    await expect(picker.getByText('Team › #general', { exact: true })).toBeVisible()
    await expect(picker.getByText('Team › #support · includes descendants', { exact: true })).toBeVisible()
    await picker.getByLabel('Search resources').fill('support')
  }

  await automation.selectOption('full')
  await page.getByLabel('Supply run credentials from Automation', { exact: true }).check()
  await expect(page.getByText('Current supplier: Automation.', { exact: false }).first()).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-editor.png'), fullPage: true })
  const original = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(original.grants).toEqual([])
  await page.setViewportSize({ width: 320, height: 844 })
  await page.emulateMedia({ colorScheme: 'dark' })
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  const review = page.getByRole('dialog', { name: 'Review before saving' })
  await expect(review).toContainText('channel support and descendants')
  await expect(review).toContainText('Run credential suppliers: Automation')
  await page.screenshot({ path: testInfo.outputPath('agent-plugin-grants-review.png'), fullPage: true })
  await review.getByRole('button', { name: 'Back to editing' }).click()
  await workspace.api(`/api/agents/${agent.id}/plugin-grants`, 'PUT', {
    agentId: agent.id,
    revision: original.revision,
    grants: [{ instanceId: remote, access: { type: 'selected', contributions: [{ kind: 'tool', name: 'grant_ui_read', resources: [] }] } }],
    runCredentialInstances: [],
  })
  const concurrentGrant = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(concurrentGrant.grants).toHaveLength(1)
  expect(concurrentGrant.grants[0]).toMatchObject({ instanceId: remote })
  // A concurrent profile edit shares the grant revision. Reload preserves this grant draft.
  await workspace.api(`/api/agents/${agent.id}`, 'PUT', { description: 'Changed elsewhere' })
  const afterProfileEdit = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(afterProfileEdit.grants).toContainEqual(concurrentGrant.grants[0])
  await page.getByRole('region', { name: 'Unsaved changes' }).getByRole('button', { name: 'Save', exact: true }).click()
  await page.getByRole('button', { name: 'Reload', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Reload', exact: true })).toHaveCount(0)
  await expect(page.getByRole('combobox', { name: 'Access to Review' })).toHaveValue('selected')
  await saveAgent(page, true)
  const saved = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  expect(saved.grants).toEqual(expect.arrayContaining([
    { instanceId: first, access: { type: 'selected', contributions: expect.arrayContaining(['tool', 'command', 'hook'].map(kind => ({ kind, name: kind === 'tool' ? 'grant_ui_read' : kind === 'command' ? 'post' : 'message', resources: [{ resourceType: 'channel', id: 'support' }] }))) } },
    { instanceId: second, access: { type: 'full' } },
  ]))
  expect(saved.runCredentialInstances).toEqual([second])
  expect(saved.grants).toContainEqual({
    instanceId: remote,
    access: { type: 'selected', contributions: [{ kind: 'tool', name: 'grant_ui_read', resources: [] }] },
  })
  await page.reload()
  await expect(support).toHaveValue('selected')
  await automation.selectOption('none')
  await saveAgent(page, true)
  expect((await workspace.api(`/api/agents/${agent.id}/plugin-grants`)).runCredentialInstances).toEqual([])
  await page.goto(workspacePath('/agents'))
  await expect(page.getByText('Selected plugin access', { exact: false })).toBeVisible()
})
