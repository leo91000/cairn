import type { OAuthClientProvider } from '@modelcontextprotocol/client'
import type { McpConnection, McpView } from '../../../../packages/contracts/mcp.ts'
import type { Config } from './config.ts'
import type { Agent, Run } from './contracts.ts'
import type { Store } from './store.ts'
import { createHash, randomBytes, randomUUID } from 'node:crypto'
import { Client, StreamableHTTPClientTransport, UnauthorizedError } from '@modelcontextprotocol/client'
import { StdioClientTransport } from '@modelcontextprotocol/client/stdio'
import { mcpInput } from '../../../../packages/contracts/mcp.ts'
import { version } from '../../../../packages/contracts/version.ts'
import { AppError, requireValue } from './errors.ts'
import { mcpFetch } from './mcp-fetch.ts'
import { McpVault } from './mcp-vault.ts'
import { policy } from './policy.ts'
import { toolkitEnvironment } from './toolkit.ts'

type Tokens = Parameters<OAuthClientProvider['saveTokens']>[0]

type ClientInfo = NonNullable<Awaited<ReturnType<OAuthClientProvider['clientInformation']>>>

type Discovery = Parameters<NonNullable<OAuthClientProvider['saveDiscoveryState']>>[0]

interface Secrets {
  token?: string
  clientSecret?: string
  env?: Record<string, string>
  tokens?: Tokens
  client?: ClientInfo
  discovery?: Discovery
  verifier?: string
  tokenExpiresAt?: number
}

interface Pending {
  connectionId: string
  revision: number
  session: string
  nonce: string
}

interface Grant {
  runId: string
  servers: Record<string, {
    revision: number
    tools: string[] | null
  }>
}

export const digest = (value: string) => createHash('sha256').update(value).digest('hex')
const serverKey = (id: string) => `cairn_${id.replaceAll('-', '_')}`

function toml(value: unknown): string {
  if (Array.isArray(value))
    return `[${value.map(toml).join(',')}]`
  if (value && typeof value === 'object')
    return `{${Object.entries(value).map(([key, item]) => `${JSON.stringify(key)}=${toml(item)}`).join(',')}}`
  return JSON.stringify(value)
}

export class McpConnections {
  readonly vault: McpVault
  private locks = new Map<string, Promise<unknown>>()
  constructor(readonly store: Store, readonly config: Config) { this.vault = new McpVault(store, config.dataDir) }
  async exclusive<T>(id: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.locks.get(id) || Promise.resolve()
    const next = previous.catch(() => { }).then(operation)
    this.locks.set(id, next)
    try {
      return await next
    }
    finally {
      if (this.locks.get(id) === next)
        this.locks.delete(id)
    }
  }

  get(id: string) { return requireValue(this.store.get('mcps', id), 'MCP connection not found.') }
  secrets(id: string) { return this.vault.get<Secrets>(id) || {} }
  changeSecrets(id: string, patch: Partial<Secrets>) { this.vault.set(id, { ...this.secrets(id), ...patch }) }
  view(item: McpConnection): McpView {
    const secret = this.secrets(item.id)
    return {
      ...item,
      hasToken: !!secret.token,
      hasClientSecret: !!secret.clientSecret,
      envKeys: Object.keys(secret.env || {}),
      callbackUrl: `${this.config.publicUrl}/oauth/mcp/callback`,
    }
  }

  list() { return this.store.list('mcps').map(item => this.view(item)) }

  assertManagementAvailable(id: string) {
    const item = this.get(id)
    if (item.transport !== 'http' || !this.locks.has(id))
      return
    const url = new URL(item.url)
    if (url.origin === this.config.publicUrl && url.pathname === '/mcp')
      throw new AppError(409, 'This self-connection is serving an active request. Manage other connections here; test or change this connection directly from the MCPs UI after the request finishes.')
  }

  async save(value: unknown, id: string = randomUUID()) {
    const input = mcpInput.parse(value)
    return this.exclusive(id, async () => {
      const existing = this.store.get('mcps', id)
      const {
        token,
        clientSecret,
        env,
        removeEnv,
        ...settings
      } = input
      const authChanged = existing && ['url', 'transport', 'auth', 'clientId', 'scopes'].some(key => existing[key as keyof McpConnection] !== settings[key as keyof typeof settings])
      const secrets = authChanged ? {} : this.secrets(id)
      const nextSecrets = {
        ...secrets,
        ...(token !== undefined ? { token } : {}),
        ...(clientSecret !== undefined ? { clientSecret } : {}),
        ...(env !== undefined ? { env: { ...secrets.env, ...env } } : {}),
      }
      for (const key of removeEnv || [])
        delete nextSecrets.env?.[key]
      if (settings.auth === 'bearer' && !nextSecrets.token)
        throw new AppError(400, 'Enter a bearer token.')
      if (settings.transport === 'stdio' && Object.keys(nextSecrets.env || {}).some(key => /^(?:HOME|CODEX_HOME|PATH|LD_PRELOAD|LD_LIBRARY_PATH|NODE_OPTIONS)$|^(?:CAIRN_|RUNNER_)/.test(key)))
        throw new AppError(400, 'Environment variables cannot override the agent runtime or home.')
      const item: McpConnection = {
        ...settings,
        id,
        createdAt: existing?.createdAt ?? Date.now(),
        revision: (existing?.revision || 0) + 1,
        state: 'untested',
        tools: existing?.tools || [],
        checkedAt: null,
        error: '',
      }
      this.store.transaction(() => {
        this.store.put('mcps', item)
        this.vault.set(id, nextSecrets)
        this.cancelPending(id)
      })
      this.store.audit('mcp.saved', { id })
      return this.view(item)
    })
  }

  cancelPending(id: string) {
    for (const entry of this.store.keys('mcp-oauth:')) {
      if ((entry.data as Pending).connectionId === id)
        this.store.delete(entry.key)
    }
  }

  async disconnect(id: string, remove = false) {
    return this.exclusive(id, async () => {
      const item = this.get(id)
      this.cancelPending(id)
      this.vault.delete(id)
      if (remove) {
        this.store.transaction(() => {
          this.store.remove('mcps', id)
          for (const agent of this.store.list('agents')) {
            const access = policy(agent)
            if (access.mcps !== null)
              access.mcps = access.mcps.filter(value => value !== id)
            delete access.mcpTools[id]
            this.store.put('agents', { ...agent, access })
          }
        })
      }
      else {
        this.store.put('mcps', {
          ...item,
          revision: item.revision + 1,
          state: 'untested',
          error: '',
          checkedAt: null,
        })
      }

      this.store.audit(remove ? 'mcp.deleted' : 'mcp.disconnected', { id })
    })
  }

  provider(item: McpConnection, interactive?: {
    nonce: string
    redirect?: string
  }): OAuthClientProvider {
    return {
      redirectUrl: `${this.config.publicUrl}/oauth/mcp/callback`,
      clientMetadata: {
        client_name: 'Cairn',
        redirect_uris: [`${this.config.publicUrl}/oauth/mcp/callback`],
        grant_types: ['authorization_code', 'refresh_token'],
        response_types: ['code'],
        token_endpoint_auth_method: item.clientId && this.secrets(item.id).clientSecret ? 'client_secret_post' : 'none',
        ...(item.scopes ? { scope: item.scopes } : {}),
      },
      state: () => {
        if (!interactive)
          throw new UnauthorizedError()
        return interactive.nonce
      },
      clientInformation: () => item.clientId ? { client_id: item.clientId, ...(this.secrets(item.id).clientSecret ? { client_secret: this.secrets(item.id).clientSecret } : {}) } : this.secrets(item.id).client,
      saveClientInformation: client => this.changeSecrets(item.id, { client }),
      tokens: () => this.secrets(item.id).tokens,
      saveTokens: tokens => this.changeSecrets(item.id, { tokens, tokenExpiresAt: tokens.expires_in ? Date.now() + tokens.expires_in * 1000 : undefined }),
      redirectToAuthorization: (url) => {
        if (!interactive)
          throw new UnauthorizedError()
        if (!['https:', 'http:'].includes(url.protocol) || (!item.allowPrivateNetwork && url.protocol !== 'https:'))
          throw new Error('Unsupported authorization URL.')
        interactive.redirect = url.toString()
      },
      saveCodeVerifier: verifier => this.changeSecrets(item.id, { verifier }),
      codeVerifier: () => requireValue(this.secrets(item.id).verifier, 'Authorization session expired.'),
      discoveryState: () => this.secrets(item.id).discovery,
      saveDiscoveryState: discovery => this.changeSecrets(item.id, { discovery }),
      invalidateCredentials: (scope) => {
        const secret = this.secrets(item.id)
        if (scope === 'all') {
          delete secret.tokens
          delete secret.client
          delete secret.discovery
          delete secret.verifier
        }

        if (scope === 'tokens')
          delete secret.tokens
        if (scope === 'client')
          delete secret.client
        if (scope === 'verifier')
          delete secret.verifier
        if (scope === 'discovery')
          delete secret.discovery
        this.vault.set(item.id, secret)
      },
    }
  }

  transport(item: McpConnection, interactive?: {
    nonce: string
    redirect?: string
  }) {
    const secret = this.secrets(item.id)
    return new StreamableHTTPClientTransport(new URL(item.url), {
      ...(item.auth === 'oauth' ? { authProvider: this.provider(item, interactive) } : {}),
      requestInit: { headers: item.auth === 'bearer' ? { Authorization: `Bearer ${secret.token || ''}` } : {} },
      fetch: mcpFetch(item.allowPrivateNetwork),
    })
  }

  async withClient<T>(item: McpConnection, fn: (client: Client) => Promise<T>, interactive?: {
    nonce: string
    redirect?: string
  }) {
    const client = new Client({ name: 'cairn-mcp-client', version })
    const env = item.transport === 'stdio' ? await toolkitEnvironment(this.config.home) : undefined
    const transport = item.transport === 'http'
      ? this.transport(item, interactive)
      : new StdioClientTransport({
          command: item.command,
          args: item.args,
          cwd: this.config.home,
          env: { PATH: env?.PATH || '/usr/local/bin:/usr/bin:/bin', HOME: this.config.home, ...this.secrets(item.id).env },
          stderr: 'pipe',
        })
    if (transport instanceof StdioClientTransport)
      transport.stderr?.on('data', () => { })
    try {
      await client.connect(transport, { timeout: 20000 })
      return await fn(client)
    }
    finally {
      await client.close().catch(() => { })
      await transport.close().catch(() => { })
    }
  }

  async discover(client: Client) {
    const tools: McpConnection['tools'] = []
    if (!client.getServerCapabilities()?.tools)
      return tools
    let cursor: string | undefined
    const seen = new Set<string>()
    do {
      const page = await client.listTools(cursor ? { cursor } : undefined, { timeout: 20000 })
      tools.push(...page.tools)
      cursor = page.nextCursor
      if (tools.length > 1000 || (cursor && seen.has(cursor)))
        throw new Error('Tool catalog is too large or has an invalid cursor.')
      if (cursor)
        seen.add(cursor)
    } while (cursor)

    return tools
  }

  failure(item: McpConnection, error: unknown) {
    const needsAuth = error instanceof UnauthorizedError
    const known = ['Private network access is disabled for this connection.', 'The endpoint redirects. Configure its final URL.', 'Instance metadata endpoints are unavailable.']
    const message = error instanceof Error && known.includes(error.message) ? error.message : needsAuth ? 'Sign in to connect this server.' : 'Could not connect. Check the endpoint, credentials, and server availability.'
    this.store.put('mcps', {
      ...item,
      state: needsAuth ? 'needs-auth' : 'error',
      error: message,
      checkedAt: Date.now(),
    })
  }

  async test(id: string) {
    return this.exclusive(id, async () => {
      const item = this.get(id)
      try {
        const tools = await this.withClient(item, client => this.discover(client))
        this.store.put('mcps', {
          ...item,
          tools,
          state: 'connected',
          error: '',
          checkedAt: Date.now(),
        })
      }
      catch (error) {
        this.failure(item, error)
      }

      return this.view(this.get(id))
    })
  }

  async connect(id: string, session: string) {
    return this.exclusive(id, async () => {
      const item = this.get(id)
      if (item.auth !== 'oauth' || item.transport !== 'http')
        throw new AppError(400, 'This connection does not use OAuth.')
      this.cancelPending(id)
      const interactive: {
        nonce: string
        redirect?: string
      } = { nonce: randomBytes(32).toString('base64url') }
      // Explicit reconnect requests fresh consent, while ordinary tool calls reuse/refresh tokens.
      this.changeSecrets(id, { tokens: undefined, verifier: undefined, discovery: undefined })
      this.store.put('mcps', {
        ...item,
        state: 'needs-auth',
        error: '',
        checkedAt: null,
      })
      try {
        await this.withClient(item, client => this.discover(client), interactive)
      }
      catch (error) {
        if (!interactive.redirect) {
          this.failure(item, error)
          throw new AppError(400, 'OAuth could not start. Check the server URL and OAuth client registration.')
        }
      }

      if (!interactive.redirect)
        throw new AppError(400, 'The server did not request OAuth. Use Test connection or choose no authentication.')
      this.store.set(`mcp-oauth:${digest(interactive.nonce)}`, {
        connectionId: id,
        revision: item.revision,
        session: digest(session),
        nonce: interactive.nonce,
      }, Date.now() + 10 * 60000)
      return { url: interactive.redirect }
    })
  }

  async callback(params: URLSearchParams, session: string) {
    const state = params.get('state') || ''
    const key = `mcp-oauth:${digest(state)}`
    const pending = this.store.kv<Pending>(key)
    if (!pending || pending.session !== digest(session))
      throw new AppError(400, 'Authorization session expired. Start again from MCPs.')
    return this.exclusive(pending.connectionId, async () => {
      if (!this.store.kv(key))
        throw new AppError(400, 'Authorization has already been completed.')
      this.store.delete(key)
      const item = this.get(pending.connectionId)
      if (item.revision !== pending.revision)
        throw new AppError(400, 'Connection settings changed. Start authorization again.')
      if (params.has('error'))
        return 'denied'
      const code = params.get('code')
      if (!code || code.length > 10000)
        throw new AppError(400, 'Missing authorization code.')
      const transport = this.transport(item)
      try {
        await transport.finishAuth(code, params.get('iss') || undefined)
        this.changeSecrets(item.id, { verifier: undefined })
        const tools = await this.withClient(item, client => this.discover(client))
        this.store.put('mcps', {
          ...item,
          state: 'connected',
          tools,
          error: '',
          checkedAt: Date.now(),
        })
        return 'connected'
      }
      catch (error) {
        this.failure(item, error)
        return 'failed'
      }
      finally {
        await transport.close().catch(() => { })
      }
    })
  }

  allowed(agent: Agent) {
    const access = policy(agent).mcps
    return this.store.list('mcps').filter(item => item.enabled && (access === null || access.includes(item.id)))
  }

  runConfiguration(run: Run) {
    const items = this.allowed(run.snapshot.agent)
    const token = randomBytes(32).toString('base64url')
    const configs: Record<string, unknown> = {}
    const servers: Grant['servers'] = {}
    const agentTools = policy(run.snapshot.agent).mcpTools
    for (const item of items) {
      const selected = agentTools[item.id]
      const tools = selected === undefined ? item.enabledTools : selected.filter(name => item.enabledTools === null || item.enabledTools.includes(name))
      if (item.transport === 'http') {
        servers[item.id] = { revision: item.revision, tools }
        configs[serverKey(item.id)] = { url: `${this.config.publicUrl}/mcp-gateway/${item.id}`, bearer_token_env_var: 'CAIRN_MCP_RUN_TOKEN', ...(tools !== null ? { enabled_tools: tools } : {}) }
      }
      else {
        configs[serverKey(item.id)] = {
          command: item.command,
          args: item.args,
          env: this.secrets(item.id).env || {},
          ...(tools !== null ? { enabled_tools: tools } : {}),
        }
      }
    }

    if (Object.keys(servers).length)
      this.store.set(`mcp-grant:${digest(token)}`, { runId: run.id, servers }, run.snapshot.agent.timeoutMinutes > 0 ? Date.now() + run.snapshot.agent.timeoutMinutes * 60000 : undefined)
    return { redactions: [token, ...items.flatMap(item => Object.values(this.secrets(item.id).env || {}))].filter(value => value.length > 3), args: Object.entries(configs).flatMap(([key, value]) => ['-c', `mcp_servers.${key}=${toml(value)}`]), env: Object.keys(servers).length ? { CAIRN_MCP_RUN_TOKEN: token } : {} }
  }

  grant(id: string, bearer: string | undefined) {
    const grant = bearer && this.store.kv<Grant>(`mcp-grant:${digest(bearer)}`)
    const run = grant && this.store.run(grant.runId)
    const scope = grant && grant.servers[id]
    const item = this.store.get('mcps', id)
    if (!scope || !run || run.status !== 'running' || !item?.enabled || item.revision !== scope.revision)
      throw new AppError(401, 'MCP run access expired or was revoked.')
    const currentAgent = this.store.get('agents', run.snapshot.agent.id)
    if (!currentAgent || JSON.stringify(policy(currentAgent)) !== JSON.stringify(policy(run.snapshot.agent)))
      throw new AppError(403, 'Agent permissions changed.')
    return { item, scope }
  }

  async proxy<T>(id: string, bearer: string | undefined, operation: (client: Client, tools: string[] | null) => Promise<T>) {
    return this.exclusive(id, async () => {
      const { item, scope } = this.grant(id, bearer)
      try {
        return await this.withClient(item, client => operation(client, scope.tools))
      }
      catch (error) {
        if (error instanceof AppError)
          throw error
        this.failure(item, error)
        throw new AppError(502, 'MCP request failed. Review the connection in MCPs.')
      }
    })
  }

  revokeRun(runId: string) {
    for (const { key, data } of this.store.keys('mcp-grant:')) {
      if ((data as Grant).runId === runId)
        this.store.delete(key)
    }
  }
}
