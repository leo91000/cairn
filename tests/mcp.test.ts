import type { Agent } from '../shared/contracts.ts'
import type { McpView } from '../shared/mcp.ts'
import path from 'node:path'
import process from 'node:process'
import {
  Client,
  StreamableHTTPClientTransport,
} from '@modelcontextprotocol/client'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { fixture } from './helpers.ts'
import { mcpProvider } from './mcp-provider.ts'

function toolResult<T = unknown>(response: Awaited<ReturnType<Client['callTool']>>): T {
  expect(response.isError).not.toBe(true)
  return (response.structuredContent as { result: T }).result
}

describe('stateless MCP over real HTTP', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  let address: string
  const clients: Client[] = []
  beforeEach(async () => {
    ctx = await fixture()
    address = await ctx.app.listen({ host: '127.0.0.1', port: 0 })
  })
  afterEach(async () => {
    await Promise.all(clients.splice(0).map(client => client.close()))
    await ctx.dispose()
  })

  async function connect(scopes: string[], protocolVersion = '2026-07-28') {
    const { token } = ctx.auth.personal('SDK test', scopes)
    const client = new Client(
      { name: 'test-client', version: '1.0.0' },
      protocolVersion === '2026-07-28'
        ? { versionNegotiation: { mode: { pin: '2026-07-28' } } }
        : { supportedProtocolVersions: [protocolVersion] },
    )
    clients.push(client)
    const transport = new StreamableHTTPClientTransport(
      new URL(`${address}/mcp`),
      { authProvider: { token: async () => token } },
    )
    await client.connect(transport)
    return { client, transport, token }
  }

  it('discovers tools and uses independent requests without a session ID', async () => {
    const { client, transport } = await connect(['read', 'run', 'manage'])
    const tools = await client.listTools()
    expect(tools.tools.map(tool => tool.name)).toEqual(
      expect.arrayContaining([
        'list_agents',
        'create_task',
        'run_task',
        'list_skills',
        'list_mcps',
        'create_mcp',
        'update_mcp',
        'test_mcp',
        'disconnect_mcp',
        'delete_mcp',
        'update_agent',
      ]),
    )
    expect(transport.sessionId).toBeUndefined()
    const result = await client.callTool({
      name: 'list_agents',
      arguments: {},
    })
    expect(result.isError).not.toBe(true)
    expect(JSON.stringify(result.content)).toContain('Test agent')
    const other = await connect(['read'])
    expect((await other.client.listTools()).tools.length).toBe(
      tools.tools.length,
    )
    const queued = await client.callTool({
      name: 'run_task',
      arguments: { taskId: ctx.task.id },
    })
    expect(queued.isError).not.toBe(true)
    expect(ctx.service.store.active()).toHaveLength(1)
  })

  it('manages command connections and existing agent access without exposing secrets', async () => {
    const { client } = await connect(['read', 'manage'])
    const created = await client.callTool({
      name: 'create_mcp',
      arguments: {
        name: 'Command tools',
        transport: 'stdio',
        command: process.execPath,
        args: [path.resolve('tests/fixtures/mcp.mjs')],
        env: { TEST_PREFIX: 'private-command-prefix' },
      },
    })
    expect(created.isError).not.toBe(true)
    expect(JSON.stringify(created)).not.toContain('private-command-prefix')
    const item = toolResult<McpView>(created)
    expect(item).toMatchObject({ state: 'untested', envKeys: ['TEST_PREFIX'] })
    expect(toolResult(created)).toHaveProperty('managementUrl', `${ctx.service.config.publicUrl}/mcps`)
    const tested = await client.callTool({ name: 'test_mcp', arguments: { id: item.id } })
    expect(tested.isError).not.toBe(true)
    expect(toolResult(tested)).toMatchObject({ state: 'connected', tools: [expect.objectContaining({ name: 'fixture_echo' })] })
    const access = { ...ctx.agent.access, mcps: [item.id], mcpTools: { [item.id]: ['fixture_echo'] } }
    const updated = await client.callTool({ name: 'update_agent', arguments: { id: ctx.agent.id, agent: { access } } })
    expect(updated.isError).not.toBe(true)
    expect(toolResult(updated)).toMatchObject({ ...ctx.agent, access })
    const renamed = await client.callTool({ name: 'update_agent', arguments: { id: ctx.agent.id, agent: { name: 'Renamed agent' } } })
    expect(renamed.isError).not.toBe(true)
    expect(toolResult<Agent>(renamed).access).toEqual(access)
    const saved = await client.callTool({ name: 'update_mcp', arguments: { id: item.id, connection: { ...item, name: 'Renamed tools' } } })
    expect(saved.isError).not.toBe(true)
    expect(ctx.service.mcps.secrets(item.id).env?.TEST_PREFIX).toBe('private-command-prefix')
    const listed = await client.callTool({ name: 'list_mcps', arguments: {} })
    expect(JSON.stringify(listed)).not.toContain('private-command-prefix')
    expect(toolResult(listed)).toEqual([expect.objectContaining({ id: item.id, name: 'Renamed tools' })])
    expect((await client.callTool({ name: 'disconnect_mcp', arguments: { id: item.id } })).isError).not.toBe(true)
    expect(ctx.service.mcps.secrets(item.id)).toEqual({})
    expect((await client.callTool({ name: 'delete_mcp', arguments: { id: item.id } })).isError).not.toBe(true)
    expect(ctx.service.mcps.list()).toEqual([])
    expect(ctx.service.store.get('agents', ctx.agent.id)!.access).toMatchObject({ mcps: [], mcpTools: {} })
    expect((await client.callTool({ name: 'update_mcp', arguments: { id: item.id, connection: item } })).isError).toBe(true)
    expect(ctx.service.mcps.list()).toEqual([])
  })

  it('creates OAuth connections through MCP and completes consent in the authenticated browser', async () => {
    const provider = await mcpProvider()
    try {
      const { client } = await connect(['read', 'manage'])
      const result = await client.callTool({
        name: 'create_mcp',
        arguments: {
          name: 'OAuth tools',
          url: `${provider.origin}/mcp`,
          auth: 'oauth',
          allowPrivateNetwork: true,
        },
      })
      expect(result.isError).not.toBe(true)
      const item = toolResult<McpView>(result)
      expect(provider.exchanges).toBe(0)
      const headers = await ctx.login()
      const started = await ctx.app.inject({ method: 'POST', url: `/api/mcps/${item.id}/connect`, headers })
      expect(started.statusCode).toBe(200)
      const authorized = await fetch(started.json().url, { redirect: 'manual' })
      expect(authorized.status).toBe(302)
      const callback = new URL(authorized.headers.get('location')!)
      expect((await ctx.app.inject({ url: `${callback.pathname}${callback.search}` })).headers.location).toContain('expired')
      const completed = await ctx.app.inject({ url: `${callback.pathname}${callback.search}`, headers })
      expect(completed.headers.location).toBe('/mcps?oauth=connected')
      const tested = await client.callTool({ name: 'test_mcp', arguments: { id: item.id } })
      expect(toolResult(tested)).toMatchObject({ state: 'connected' })
      expect(JSON.stringify(tested)).not.toContain('fixture-access-token')
      provider.expire()
      expect(toolResult(await client.callTool({ name: 'test_mcp', arguments: { id: item.id } }))).toMatchObject({ state: 'connected' })
      expect(provider.refreshes).toBe(1)
    }
    finally { await provider.close() }
  })

  it('requires manage scope for every MCP mutation and command discovery', async () => {
    const { client } = await connect(['read', 'run'])
    const item = await ctx.service.mcps.save({
      name: 'Protected',
      transport: 'stdio',
      command: 'does-not-exist',
      env: { PRIVATE_TOKEN: 'private-token-value' },
    })
    const tools = await client.listTools()
    for (const [name, args] of [
      ['create_mcp', { name: 'Forbidden', transport: 'stdio', command: 'node' }],
      ['update_mcp', { id: item.id, connection: item }],
      ['test_mcp', { id: item.id }],
      ['disconnect_mcp', { id: item.id }],
      ['delete_mcp', { id: item.id }],
      ['update_agent', { id: ctx.agent.id, agent: { access: { mcps: [] } } }],
    ] as const) {
      const denied = await client.callTool({ name, arguments: args })
      expect(denied.isError, name).toBe(true)
      expect(denied._meta).toHaveProperty('mcp/www_authenticate')
      expect(tools.tools.find(tool => tool.name === name)?.annotations?.readOnlyHint).toBe(false)
    }

    expect(ctx.service.mcps.get(item.id)).toMatchObject({ state: 'untested', revision: 1 })
    expect(ctx.service.mcps.secrets(item.id).env?.PRIVATE_TOKEN).toBe('private-token-value')
    expect(ctx.service.store.get('agents', ctx.agent.id)).toEqual(ctx.agent)
    const list = await client.callTool({ name: 'list_mcps', arguments: {} })
    expect(list.isError).not.toBe(true)
    expect(JSON.stringify(list)).not.toContain('private-token-value')
    expect(tools.tools.find(tool => tool.name === 'test_mcp')?.annotations?.destructiveHint).toBe(true)
  })

  it('manages other connections through its own gateway and rejects self-locking operations promptly', async () => {
    ctx.service.config.publicUrl = address
    ctx.auth.publicUrl = address
    const { token } = ctx.auth.personal('Self connection', ['read', 'manage'])
    const self = await ctx.service.mcps.save({
      name: 'Self',
      url: `${address}/mcp?source=self-test`,
      auth: 'bearer',
      token,
      allowPrivateNetwork: true,
    })
    const run = await ctx.service.enqueue(ctx.task.id)
    ctx.service.store.updateRun(run.id, { status: 'running' })
    const config = ctx.service.mcps.runConfiguration(run)
    const client = new Client({ name: 'gateway-agent', version: '1' })
    clients.push(client)
    await client.connect(new StreamableHTTPClientTransport(new URL(`${address}/mcp-gateway/${self.id}`), { requestInit: { headers: { Authorization: `Bearer ${config.env.LEO_MCP_RUN_TOKEN}` } } }))
    const created = toolResult<McpView>(await client.callTool({ name: 'create_mcp', arguments: { name: 'Other tools', url: 'https://example.com/mcp', enabled: false } }))
    expect(created.state).toBe('untested')
    for (const [name, args] of [
      ['test_mcp', { id: self.id }],
      ['update_mcp', { id: self.id, connection: self }],
      ['disconnect_mcp', { id: self.id }],
      ['delete_mcp', { id: self.id }],
    ] as const) {
      const result = await client.callTool({ name, arguments: args }, { timeout: 2000 })
      expect(result.isError, name).toBe(true)
      expect(JSON.stringify(result)).toContain('self-connection is serving an active request')
    }

    expect(toolResult(await client.callTool({ name: 'delete_mcp', arguments: { id: created.id } }))).toEqual({ deleted: true })
    expect(ctx.service.mcps.get(self.id)).toMatchObject({ revision: 1 })
    expect((await ctx.service.mcps.test(self.id)).state).toBe('connected')
  }, 10000)

  it.each(['2026-07-28', '2025-06-18'])('enforces scopes and revocation over %s', async (protocolVersion) => {
    const { client, token } = await connect(['read'], protocolVersion)
    const denied = await client.callTool({
      name: 'run_task',
      arguments: { taskId: ctx.task.id },
    })
    expect(denied.isError).toBe(true)
    expect(ctx.service.store.active()).toHaveLength(0)
    const unauthorized = await fetch(`${address}/mcp`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'tools/list' }),
    })
    expect(unauthorized.status).toBe(401)
    expect(unauthorized.headers.get('www-authenticate')).toContain(
      'oauth-protected-resource/mcp',
    )
    ctx.auth.revoke(ctx.auth.verify(token).family)
    await expect(
      client.callTool({ name: 'list_agents', arguments: {} }),
    ).rejects.toThrow()
  })

  it.each(['2025-06-18', '2025-11-25'])('supports %s clients without session state', async (protocolVersion) => {
    const { client, transport, token } = await connect(['read'], protocolVersion)
    expect(transport.sessionId).toBeUndefined()
    expect((await client.listTools()).tools.map(tool => tool.name)).toContain('list_agents')
    const other = await connect(['read'], protocolVersion)
    expect(other.transport.sessionId).toBeUndefined()
    const result = await other.client.callTool({ name: 'list_agents', arguments: {} })
    expect(result.isError).not.toBe(true)
    expect(JSON.stringify(result.content)).toContain('Test agent')

    const initialized = await fetch(`${address}/mcp`, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        'accept': 'application/json, text/event-stream',
        'authorization': `Bearer ${token}`,
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion,
          capabilities: {},
          clientInfo: { name: 'old', version: '1' },
        },
      }),
    })
    expect(initialized.status).toBe(200)
    expect(initialized.headers.get('mcp-session-id')).toBeNull()
    await initialized.text()
    for (const method of ['GET', 'DELETE']) {
      const response = await fetch(`${address}/mcp`, {
        method,
        headers: {
          'authorization': `Bearer ${token}`,
          'accept': 'application/json, text/event-stream',
          'mcp-protocol-version': protocolVersion,
        },
      })
      expect(response.status).toBe(405)
      await response.text()
    }
  })
})
