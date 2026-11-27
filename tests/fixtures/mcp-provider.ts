import { createHash } from 'node:crypto'
import formbody from '@fastify/formbody'
import { toNodeHandler } from '@modelcontextprotocol/node'
import { createMcpHandler, McpServer } from '@modelcontextprotocol/server'
import Fastify from 'fastify'
import { z } from 'zod'

export async function mcpProvider(options: {
  clientSecret?: string
  beforeAuthorize?: () => Promise<void>
  perToolAuthorization?: boolean
  dynamicRegistration?: boolean
  resourceMetadata?: 'advertised' | 'query' | 'unadvertised-query' | 'root' | 'root-query' | 'wrong-resource' | 'different-spelling'
} = {}) {
  // The manager outlives this fixture and can retain HTTP/SSE connections.
  // Closing the provider must also close those sockets, without waiting for the client.
  const app = Fastify({ forceCloseConnections: true })
  await app.register(formbody)
  let origin = ''
  let challenge = ''
  let accessToken = 'fixture-access-token'
  let refreshes = 0
  let exchanges = 0
  const metadataRoot = '/.well-known/oauth-protected-resource'
  const usesRootMetadata = options.resourceMetadata === 'root' || options.resourceMetadata === 'root-query'
  const metadataPath = options.resourceMetadata === 'advertised'
    ? '/resource-metadata'
    : usesRootMetadata ? metadataRoot : `${metadataRoot}/mcp`
  const resourcePath = usesRootMetadata || options.resourceMetadata === 'wrong-resource'
    ? ''
    : options.resourceMetadata === 'different-spelling' ? '/tenant/../mcp' : '/mcp'
  const resourceQuery = options.resourceMetadata === 'query' || options.resourceMetadata === 'unadvertised-query' || options.resourceMetadata === 'root-query' ? '?tenant=fixture' : ''
  const metadata = () => ({ resource: `${origin}${resourcePath}${resourceQuery}`, authorization_servers: [origin], scopes_supported: ['tools:read'] })
  app.get(metadataPath, (request, reply) => {
    if (request.url !== `${metadataPath}${resourceQuery}`)
      return reply.code(404).send()
    return metadata()
  })
  if (!options.resourceMetadata)
    app.get(metadataRoot, metadata)
  app.get('/.well-known/oauth-authorization-server', () => ({
    issuer: origin,
    authorization_endpoint: `${origin}/authorize`,
    token_endpoint: `${origin}/token`,
    registration_endpoint: options.dynamicRegistration === false ? undefined : `${origin}/register`,
    response_types_supported: ['code'],
    grant_types_supported: ['authorization_code', 'refresh_token'],
    token_endpoint_auth_methods_supported: ['none', 'client_secret_post'],
    code_challenge_methods_supported: ['S256'],
    authorization_response_iss_parameter_supported: true,
  }))
  if (options.dynamicRegistration !== false)
    app.post('/register', request => ({ ...(request.body as object), client_id: 'fixture-client' }))
  app.get('/authorize', async (request, reply) => {
    await options.beforeAuthorize?.()
    const query = request.query as Record<string, string>
    challenge = query.code_challenge
    const callback = new URL(query.redirect_uri)
    callback.searchParams.set('code', 'fixture-code')
    callback.searchParams.set('state', query.state)
    callback.searchParams.set('iss', origin)
    return reply.redirect(callback.toString())
  })
  app.post('/token', (request, reply) => {
    const body = request.body as Record<string, string>
    if (options.clientSecret && (body.client_id !== 'registered-client' || body.client_secret !== options.clientSecret))
      return reply.code(401).send({ error: 'invalid_client' })
    if (body.grant_type === 'refresh_token') {
      if (body.refresh_token !== 'fixture-refresh-token')
        return reply.code(400).send({ error: 'invalid_grant' })
      refreshes++
    }
    else {
      if (body.code !== 'fixture-code' || createHash('sha256').update(body.code_verifier).digest('base64url') !== challenge)
        return reply.code(400).send({ error: 'invalid_grant' })
      exchanges++
    }

    return {
      access_token: accessToken,
      refresh_token: 'fixture-refresh-token',
      token_type: 'Bearer',
      expires_in: 3600,
      scope: 'tools:read',
    }
  })
  const handler = createMcpHandler(() => {
    const server = new McpServer({ name: 'fixture-tools', version: '1.0.0' })
    server.registerResource('guide', 'fixture://guide', { mimeType: 'text/plain' }, async uri => ({ contents: [{ uri: uri.href, text: 'Workspace guide' }] }))
    server.registerPrompt('review', { argsSchema: z.object({ subject: z.string() }) }, ({ subject }) => ({ messages: [{ role: 'user', content: { type: 'text', text: `Review ${subject}` } }] }))
    server.registerTool('echo', { title: 'Echo message', description: 'Returns your message. Used to verify connectivity.', inputSchema: z.object({ message: z.string() }) }, async ({ message }) => ({ content: [{ type: 'text', text: message }] }))
    server.registerTool('admin_reset', { title: 'Reset workspace', description: 'Restricted administrative action.', inputSchema: z.object({}) }, async () => ({ content: [{ type: 'text', text: 'reset' }] }))
    return server
  })
  app.all(options.resourceMetadata === 'root-query' ? '/' : '/mcp', async (request, reply) => {
    const rpc = request.body as { method?: string, params?: { name?: string } } | null
    const protectedTool = rpc?.method === 'tools/call' && rpc.params?.name === 'admin_reset'
    const challenge = options.resourceMetadata === 'unadvertised-query'
      ? 'Bearer'
      : `Bearer resource_metadata="${origin}${metadataPath}${resourceQuery}"`
    if ((!options.perToolAuthorization || protectedTool) && request.headers.authorization !== `Bearer ${accessToken}`)
      return reply.code(401).header('WWW-Authenticate', challenge).send({ error: 'unauthorized' })
    if (options.resourceMetadata === 'advertised' && rpc?.method === 'server/discover') {
      reply.raw.setHeader('WWW-Authenticate', [
        String.raw`Basic realm="description with resource_metadata=\"https://other.invalid/\""`,
        `Basic realm="Café", Negotiate resource_metadata=, Bearer RESOURCE_METADATA = "${origin}${metadataPath}"`,
      ])
    }

    reply.hijack()
    await toNodeHandler(handler)(request.raw, reply.raw, request.body)
  })
  await app.listen({ host: '127.0.0.1', port: 0 })
  origin = `http://127.0.0.1:${(app.server.address() as {
    port: number
  }).port}`
  return {
    app,
    origin,
    expire() {
      accessToken = 'fixture-rotated-token'
    },
    get refreshes() {
      return refreshes
    },
    get exchanges() {
      return exchanges
    },
    close: () => app.close(),
  }
}
