import { readFile, stat } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { Client, StreamableHTTPClientTransport } from '@modelcontextprotocol/client'
import { describe, expect, it } from 'vitest'
import { MAIN_AGENT_ID } from '../shared/contracts.ts'
import { fixture } from './helpers.ts'
import { isPrivateAddress, mcpFetch } from './legacy/server/mcp-fetch.ts'
import { isolated, policy } from './legacy/server/policy.ts'
import { mcpProvider } from './mcp-provider.ts'

async function consent(url: string) {
  const response = await fetch(url, { redirect: 'manual' })
  expect(response.status).toBe(302)
  return new URL(response.headers.get('location')!)
}

describe('outbound MCP connection lifecycle', () => {
  it('authorizes a pre-registered confidential OAuth client', async () => {
    const provider = await mcpProvider({ clientSecret: 'registered-secret' })
    const ctx = await fixture()
    try {
      const item = await ctx.service.mcps.save({
        name: 'Registered OAuth',
        url: `${provider.origin}/mcp`,
        auth: 'oauth',
        clientId: 'registered-client',
        clientSecret: 'registered-secret',
        allowPrivateNetwork: true,
      })
      const started = await ctx.service.mcps.connect(item.id, 'session')
      const callback = await consent(started.url)
      expect(await ctx.service.mcps.callback(callback.searchParams, 'session')).toBe('connected')
      expect(provider.exchanges).toBe(1)
    }
    finally {
      await ctx.dispose()
      await provider.close()
    }
  })
  it('discovers command servers and passes configured environment values', async () => {
    const ctx = await fixture()
    try {
      const item = await ctx.service.mcps.save({
        name: 'Local tool',
        transport: 'stdio',
        command: process.execPath,
        args: [path.resolve('tests/fixtures/mcp.mjs')],
        env: { TEST_PREFIX: 'private-prefix:' },
      })
      expect((await ctx.service.mcps.test(item.id)).state).toBe('connected')
      const result = await ctx.service.mcps.withClient(ctx.service.mcps.get(item.id), client => client.callTool({ name: 'fixture_echo', arguments: { message: 'hello' } }))
      expect(result).toMatchObject({ content: [{ text: 'private-prefix:hello' }] })
      const run = await ctx.service.enqueue(ctx.task.id)
      const config = ctx.service.mcps.runConfiguration(run)
      expect(config.args.join(' ')).toContain('TEST_PREFIX')
      expect(config.redactions).toContain('private-prefix:')
    }
    finally { await ctx.dispose() }
  })
  it('completes OAuth with PKCE, rejects replay, encrypts secrets and serializes refresh across runs', async () => {
    const provider = await mcpProvider()
    const ctx = await fixture()
    try {
      const headers = await ctx.login()
      const created = await ctx.app.inject({
        method: 'POST',
        url: '/api/mcps',
        headers,
        payload: {
          name: 'OAuth tools',
          url: `${provider.origin}/mcp`,
          auth: 'oauth',
          allowPrivateNetwork: true,
        },
      })
      expect(created.statusCode).toBe(200)
      const item = created.json()
      const connect = await ctx.app.inject({ method: 'POST', url: `/api/mcps/${item.id}/connect`, headers })
      expect(connect.statusCode, connect.body).toBe(200)
      const callback = await consent(connect.json().url)
      const missingSession = await ctx.app.inject({ url: `${callback.pathname}${callback.search}` })
      expect(missingSession.headers.location).toContain('expired')
      const completed = await ctx.app.inject({ url: `${callback.pathname}${callback.search}`, headers })
      expect(completed.headers.location).toBe('/mcps?oauth=connected')
      expect(provider.exchanges).toBe(1)
      expect((await ctx.app.inject({ url: `${callback.pathname}${callback.search}`, headers })).headers.location).toContain('expired')
      const list = await ctx.app.inject({ url: '/api/mcps', headers })
      expect(list.json()[0].tools.map((tool: {
        name: string
      }) => tool.name)).toEqual(['echo', 'admin_reset'])
      expect(list.body).not.toContain('fixture-access-token')
      const stored = ctx.service.store.db.prepare('SELECT data FROM kv WHERE key LIKE ?').all('mcp-secret:%')
      expect(JSON.stringify(stored)).not.toContain('fixture-access-token')
      expect((await stat(path.join(ctx.directory, 'data/mcp-encryption-key'))).mode & 0o777).toBe(0o600)
      provider.expire()
      const results = await Promise.all([ctx.service.mcps.test(item.id), ctx.service.mcps.test(item.id)])
      expect(results.map(result => result.state)).toEqual(['connected', 'connected'])
      expect(provider.refreshes).toBe(1)
      await ctx.service.mcps.disconnect(item.id)
      expect(ctx.service.mcps.secrets(item.id)).toEqual({})
    }
    finally {
      await ctx.dispose()
      await provider.close()
    }
  })
  it('rejects OAuth issuer substitution and superseded consent, and handles denial', async () => {
    const provider = await mcpProvider()
    const ctx = await fixture()
    try {
      const item = await ctx.service.mcps.save({
        name: 'OAuth',
        url: `${provider.origin}/mcp`,
        auth: 'oauth',
        allowPrivateNetwork: true,
      })
      const first = await ctx.service.mcps.connect(item.id, 'session')
      const stale = await consent(first.url)
      const second = await ctx.service.mcps.connect(item.id, 'session')
      await expect(ctx.service.mcps.callback(stale.searchParams, 'session')).rejects.toThrow('expired')
      const callback = await consent(second.url)
      await expect(ctx.service.mcps.callback(callback.searchParams, 'other-session')).rejects.toThrow('expired')
      callback.searchParams.set('iss', 'https://wrong-issuer.example')
      expect(await ctx.service.mcps.callback(callback.searchParams, 'session')).toBe('failed')
      expect(provider.exchanges).toBe(0)
      const third = await ctx.service.mcps.connect(item.id, 'session')
      const denied = new URL(third.url).searchParams
      denied.set('error', 'access_denied')
      expect(await ctx.service.mcps.callback(denied, 'session')).toBe('denied')
      expect(ctx.service.mcps.get(item.id).state).toBe('needs-auth')
    }
    finally {
      await ctx.dispose()
      await provider.close()
    }
  })
  it('enforces server and tool grants at the MCP boundary and revokes changed or finished runs', async () => {
    const provider = await mcpProvider()
    const ctx = await fixture()
    const client = new Client({ name: 'test-agent', version: '1' })
    try {
      await ctx.app.listen({ host: '127.0.0.1', port: 0 })
      const origin = `http://127.0.0.1:${(ctx.app.server.address() as {
        port: number
      }).port}`
      const item = await ctx.service.mcps.save({
        name: 'Token tools',
        url: `${provider.origin}/mcp`,
        auth: 'bearer',
        token: 'fixture-access-token',
        allowPrivateNetwork: true,
      })
      const agent = ctx.service.agent({ ...ctx.agent, access: { mcps: [item.id], mcpTools: { [item.id]: ['echo'] } } }, ctx.agent.id)
      expect(isolated(agent)).toBe(true)
      const run = await ctx.service.enqueue(ctx.task.id)
      ctx.service.store.updateRun(run.id, { status: 'running' })
      const config = ctx.service.mcps.runConfiguration(run)
      expect(config.args.join(' ')).not.toContain('fixture-access-token')
      expect(config.args.join(' ')).toContain('LEO_MCP_RUN_TOKEN')
      const transport = new StreamableHTTPClientTransport(new URL(`${origin}/mcp-gateway/${item.id}`), { requestInit: { headers: { Authorization: `Bearer ${config.env.LEO_MCP_RUN_TOKEN}` } } })
      await client.connect(transport)
      expect((await client.listTools()).tools.map(tool => tool.name)).toEqual(['echo'])
      expect(await client.callTool({ name: 'echo', arguments: { message: 'works' } })).toMatchObject({ content: [{ text: 'works' }] })
      expect((await client.listResources()).resources[0].uri).toBe('fixture://guide')
      expect(await client.readResource({ uri: 'fixture://guide' })).toMatchObject({ contents: [{ text: 'Workspace guide' }] })
      expect(await client.listResourceTemplates()).toEqual({ resourceTemplates: [] })
      expect((await client.listPrompts()).prompts[0].name).toBe('review')
      expect(await client.getPrompt({ name: 'review', arguments: { subject: 'changes' } })).toMatchObject({ messages: [{ content: { text: 'Review changes' } }] })
      await expect(client.callTool({ name: 'admin_reset', arguments: {} })).rejects.toThrow()
      ctx.service.store.updateRun(run.id, { status: 'succeeded' })
      expect((await ctx.app.inject({
        method: 'POST',
        url: `/mcp-gateway/${item.id}`,
        headers: { authorization: `Bearer ${config.env.LEO_MCP_RUN_TOKEN}` },
        payload: {},
      })).statusCode).toBe(401)
      ctx.service.store.updateRun(run.id, { status: 'running' })
      await ctx.service.mcps.save({ ...item, enabled: false }, item.id)
      expect(() => ctx.service.mcps.grant(item.id, config.env.LEO_MCP_RUN_TOKEN)).toThrow()
      expect(() => ctx.service.agent({ name: 'Main', access: { mcps: [] } }, MAIN_AGENT_ID)).toThrow()
      ctx.service.mcps.revokeRun(run.id)
      expect(ctx.service.store.keys('mcp-grant:')).toEqual([])
      await ctx.service.mcps.disconnect(item.id, true)
      const access = ctx.service.store.get('agents', agent.id)!.access
      expect(access.mcps).toEqual([])
      expect(access.mcpTools).toEqual({})
    }
    finally {
      await client.close()
      await ctx.dispose()
      await provider.close()
    }
  })
  it('protects configuration APIs, preserves write-only secrets, and fails closed for legacy restricted agents', async () => {
    const ctx = await fixture()
    try {
      expect((await ctx.app.inject({ url: '/api/mcps' })).statusCode).toBe(401)
      const headers = await ctx.login()
      expect((await ctx.app.inject({
        method: 'POST',
        url: '/api/mcps',
        headers: { cookie: headers.cookie },
        payload: {},
      })).statusCode).toBe(403)
      const item = await ctx.service.mcps.save({
        name: 'Command',
        transport: 'stdio',
        command: 'node',
        env: { API_TOKEN: 'secret-unique-token' },
      })
      const update = await ctx.service.mcps.save({ ...item, name: 'Renamed' }, item.id)
      expect(update.envKeys).toEqual(['API_TOKEN'])
      expect(JSON.stringify(update)).not.toContain('secret-unique-token')
      expect(ctx.service.mcps.secrets(item.id).env?.API_TOKEN).toBe('secret-unique-token')
      await ctx.service.mcps.save({ ...update, removeEnv: ['API_TOKEN'] }, item.id)
      expect(ctx.service.mcps.secrets(item.id).env).toEqual({})
      const legacy = {
        ...ctx.agent,
        access: {
          projects: [],
          skills: null,
          github: false,
          sandbox: 'yolo',
        },
      } as unknown as typeof ctx.agent
      expect(policy(legacy).mcps).toEqual([])
      const legacyMain = {
        ...ctx.agent,
        id: MAIN_AGENT_ID,
        access: {
          projects: null,
          skills: null,
          github: true,
          sandbox: 'read-only',
        },
      } as unknown as typeof ctx.agent
      expect(policy(legacyMain).mcps).toBeNull()
      await expect(ctx.service.mcps.save({
        name: 'Unsafe',
        transport: 'stdio',
        command: 'node',
        env: { HOME: '/data' },
      })).rejects.toThrow('runtime')
      expect((await readFile(path.join(ctx.directory, 'data/mcp-encryption-key'))).length).toBe(32)
    }
    finally {
      await ctx.dispose()
    }
  })
  it('blocks private and metadata endpoints unless explicitly permitted', async () => {
    for (const address of ['127.0.0.1', '10.0.0.1', '169.254.169.254', '::1', '::ffff:127.0.0.1', 'fd00::1'])
      expect(isPrivateAddress(address)).toBe(true)
    expect(isPrivateAddress('1.1.1.1')).toBe(false)
    await expect(mcpFetch(false)('http://127.0.0.1:9')).rejects.toThrow('Private network')
    await expect(mcpFetch(true)('http://169.254.169.254')).rejects.toThrow('metadata')
    await expect(mcpFetch(true)('http://[::ffff:169.254.169.254]')).rejects.toThrow('metadata')
  })
})
