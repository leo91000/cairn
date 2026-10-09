import type { FastifyInstance } from 'fastify'
import type { Auth } from './auth.ts'
import type { McpConnections } from './mcp-connections.ts'
import { toNodeHandler } from '@modelcontextprotocol/node'
import { createMcpHandler, McpServer } from '@modelcontextprotocol/server'
import { z } from 'zod'
import { version } from '../../../../packages/contracts/version.ts'
import { AppError } from './errors.ts'

export function mountMcpConnections(app: FastifyInstance, connections: McpConnections, auth: Auth) {
  const params = z.object({ id: z.string().uuid() })
  app.get('/api/mcps', () => connections.list())
  app.post('/api/mcps', request => connections.save(request.body))
  app.put('/api/mcps/:id', (request) => {
    const { id } = params.parse(request.params)
    connections.get(id)
    return connections.save(request.body, id)
  })
  app.delete('/api/mcps/:id', async (request) => {
    await connections.disconnect(params.parse(request.params).id, true)
    return { ok: true }
  })
  app.post('/api/mcps/:id/test', request => connections.test(params.parse(request.params).id))
  app.post('/api/mcps/:id/connect', request => connections.connect(params.parse(request.params).id, auth.read(request.cookies.cairn_session)!.csrf))
  app.post('/api/mcps/:id/disconnect', async (request) => {
    await connections.disconnect(params.parse(request.params).id)
    return { ok: true }
  })
  app.get('/oauth/mcp/callback', { logLevel: 'silent' }, async (request, reply) => {
    reply.header('Cache-Control', 'no-store').header('Referrer-Policy', 'no-referrer')
    const session = auth.read(request.cookies.cairn_session)
    if (!session)
      return reply.redirect('/mcps?oauth=expired')
    try {
      const result = await connections.callback(new URL(request.url, connections.config.publicUrl).searchParams, session.csrf)
      return reply.redirect(`/mcps?oauth=${result}`)
    }
    catch {
      return reply.redirect('/mcps?oauth=expired')
    }
  })
  app.all('/mcp-gateway/:id', { config: { rateLimit: { max: 600, timeWindow: '1 minute' } } }, async (request, reply) => {
    const { id } = params.parse(request.params)
    const bearer = request.headers.authorization?.match(/^Bearer (.+)$/i)?.[1]
    connections.grant(id, bearer)
    reply.header('Cache-Control', 'no-store')
    const handler = createMcpHandler(() => {
      const server = new McpServer({ name: 'cairn-mcp-gateway', version }, { capabilities: { tools: {}, resources: {}, prompts: {} } })
      server.server.setRequestHandler('tools/list', () => connections.proxy(id, bearer, async (client, allowed) => {
        const tools = await connections.discover(client)
        return { tools: tools.filter(tool => allowed === null || allowed.includes(tool.name)) }
      }))
      server.server.setRequestHandler('tools/call', request => connections.proxy(id, bearer, async (client, allowed) => {
        if (allowed !== null && !allowed.includes(request.params.name))
          throw new AppError(403, 'This tool is unavailable to this agent.')
        return await client.callTool(request.params, { timeout: 60000 })
      }))
      server.server.setRequestHandler('resources/list', request => connections.proxy(id, bearer, async client => client.getServerCapabilities()?.resources ? await client.listResources(request.params) : { resources: [] }))
      server.server.setRequestHandler('resources/templates/list', request => connections.proxy(id, bearer, async client => client.getServerCapabilities()?.resources ? await client.listResourceTemplates(request.params) : { resourceTemplates: [] }))
      server.server.setRequestHandler('resources/read', request => connections.proxy(id, bearer, client => client.readResource(request.params)))
      server.server.setRequestHandler('prompts/list', request => connections.proxy(id, bearer, async client => client.getServerCapabilities()?.prompts ? await client.listPrompts(request.params) : { prompts: [] }))
      server.server.setRequestHandler('prompts/get', request => connections.proxy(id, bearer, client => client.getPrompt(request.params)))
      return server
    })
    reply.hijack()
    await toNodeHandler(handler)(request.raw, reply.raw, request.body)
  })
}
