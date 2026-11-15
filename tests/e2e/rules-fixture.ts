import type { Workspace } from './fixtures'
import { expect } from '@playwright/test'

/** Local plugin and real installation API, never a fabricated UI or fake history response. */
const setups = new WeakMap<Workspace, ReturnType<typeof installRulesFixture>>()
export function rulesFixture(workspace: Workspace) {
  let setup = setups.get(workspace)
  if (!setup) {
    setup = installRulesFixture(workspace)
    setups.set(workspace, setup)
  }

  return setup
}

async function installRulesFixture(workspace: Workspace) {
  const prepare = async (path: string, body: unknown): Promise<any> => {
    const started = await workspace.api(path, 'POST', body)
    if (!started.operation)
      return started
    let operation: any
    await expect.poll(async () => {
      operation = await workspace.api(`/api/plugins/packages/operations/${started.operation}`)
      return operation.state
    }, { timeout: 30000 }).toBe('completed')
    return operation.result.kind === 'package' ? operation.result.version : operation.result.output.instance
  }

  const manifest = {
    id: 'rules-studio',
    name: 'Team messages',
    version: '1',
    apiVersion: 2,
    capabilities: ['hooks'],
    agentAccess: { description: 'Receive and answer team messages.' },
    resources: [{ name: 'channel', description: 'Channels' }, { name: 'person', description: 'People' }],
    hooks: [{
      name: 'message',
      description: 'Message received',
      payloadSchema: {
        type: 'object',
        properties: {
          channel: { type: 'string', title: 'Channel' },
          author: { type: 'string', title: 'Author' },
          content: { type: 'string', title: 'Message text' },
          tags: { type: 'array', items: { type: 'string' } },
          count: { type: 'integer' },
          counts: { type: 'array', items: { type: 'integer' } },
          approved: { type: 'boolean' },
        },
      },
      resourceFields: [{ path: '/channel', resourceType: 'channel' }, { path: '/author', resourceType: 'person' }],
    }, {
      name: 'nested-message',
      description: 'Message with person details',
      payloadSchema: { type: 'object', properties: { author: { type: 'object', properties: { id: { type: 'string' }, name: { type: 'string' } } }, content: { type: 'string' } } },
      resourceFields: [{ path: '/author/id', resourceType: 'person' }],
    }],
    tools: [{
      name: 'rules_studio_post_message',
      description: 'Post a message to a channel',
      inputSchema: {
        type: 'object',
        required: ['channel', 'text'],
        properties: {
          channel: { type: 'string', title: 'Channel' },
          text: { type: 'string', title: 'Message', minLength: 1 },
          body: { type: 'object', title: 'Body' },
          options: { type: 'object', title: 'Options', properties: { notify: { type: 'boolean', title: 'Notify the channel' }, style: { type: 'string', enum: ['plain', 'emphasis'], title: 'Style' } } },
          labels: { type: 'array', title: 'Labels', items: { type: 'string' } },
        },
      },
      resourceFields: [{ path: '/channel', resourceType: 'channel' }],
    }],
  }
  const source = `return {
    resources = function(input)
      local choices = if input.resourceType == "person" then {{resourceType="person",id="alice",name="Alice"},{resourceType="person",id="bob",name="Bob"}} else {{resourceType="channel",id="support",name="#support"},{resourceType="channel",id="general",name="#general"}}
      if input.op == "list" then
        local items = cairn.array({})
        for _, item in choices do if input.query == "" or string.find(string.lower(item.name),string.lower(input.query),1,true) then table.insert(items,item) end end
        return {items=items}
      end
      for _, item in choices do if item.id == input.id then return item end end
      return {}
    end,
    tool = function(input) return {ok=true,messageId="local-message",text=input.input.text,channel=input.input.channel} end
  }`
  const installed = await prepare('/api/plugins/packages', {
    manifest,
    manifestFormat: 'json',
    source,
    modules: {},
  })
  await prepare('/api/plugins/packages/rules-studio/approve', { pluginId: 'rules-studio', version: '1', digest: installed.digest })
  const saved = await prepare('/api/plugins/manage', {
    kind: 'save',
    pluginId: 'rules-studio',
    instanceId: null,
    data: {
      name: 'Support team',
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
  const instanceId = saved.instance.id as string
  const agent = workspace.service.agent({ name: 'Support assistant' })
  const grants = await workspace.api(`/api/agents/${agent.id}/plugin-grants`)
  await workspace.api(`/api/agents/${agent.id}/plugin-grants`, 'PUT', {
    agentId: agent.id,
    revision: grants.revision,
    grants: [{ instanceId, access: { type: 'full' } }],
    runCredentialInstances: [],
  })
  const definition = {
    name: 'Support messages',
    description: 'Acknowledge messages from the support team.',
    executionAgentId: agent.id,
    trigger: { type: 'plugin', instanceId, hook: 'message' },
    limits: { triggeringsPerHour: 60, conversationsPerHour: 5 },
    steps: [{
      type: 'if',
      id: 'trusted',
      condition: { type: 'authorIn', author: { type: 'formula', expression: 'author' }, people: [{ resourceType: 'person', id: 'alice' }] },
      then: [{
        type: 'action',
        id: 'post',
        label: 'Acknowledge the message',
        action: {
          type: 'callTool',
          instanceId,
          tool: 'rules_studio_post_message',
          input: { channel: { type: 'literal', value: 'support' }, text: { type: 'template', text: 'Received: {{content}}' } },
        },
      }],
      otherwise: [{
        type: 'action',
        id: 'remember',
        label: 'Keep the last author',
        action: {
          type: 'storeValue',
          scope: 'rule',
          name: 'lastAuthor',
          value: { type: 'formula', expression: 'author' },
        },
      }],
    }],
  }
  return { agent, instanceId, definition }
}
