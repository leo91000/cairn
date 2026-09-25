import type { Store } from './store.ts'
import { Buffer } from 'node:buffer'
import {
  createHash,
  randomBytes,
  scrypt as scryptCallback,
  timingSafeEqual,
} from 'node:crypto'
import { promisify } from 'node:util'
import { AppError } from './errors.ts'

const scrypt = promisify(scryptCallback)
export const token = () => randomBytes(32).toString('base64url')
export function digest(value: string) {
  return createHash('sha256').update(value).digest('base64url')
}

export function safeEqual(a: string, b: string) {
  const left = Buffer.from(a)
  const right = Buffer.from(b)
  return left.length === right.length && timingSafeEqual(left, right)
}

export interface Session {
  csrf: string
  createdAt: number
}
export interface Grant {
  clientId: string
  scopes: string[]
  resource: string
  family: string
  expiresAt: number
  label: string
  used?: boolean
}
export class OAuthError extends AppError {
  constructor(
    public code: string,
    message: string,
  ) {
    super(400, message)
  }
}
export class Auth {
  constructor(
    public store: Store,
    public publicUrl: string,
  ) {}

  async setup(password: string) {
    if (this.store.kv('admin'))
      throw new AppError(409, 'Setup is already complete.')
    if (password.length < 12 || password.length > 200)
      throw new AppError(400, 'Use a password between 12 and 200 characters.')
    const salt = token()
    const hash = ((await scrypt(password, salt, 64)) as Buffer).toString('hex')
    if (this.store.kv('admin'))
      throw new AppError(409, 'Setup is already complete.')
    this.store.set('admin', { salt, hash })
  }

  async login(password: string) {
    const admin = this.store.kv<{ salt: string, hash: string }>('admin')
    if (!admin)
      throw new AppError(401, 'Complete setup first.')
    const hash = ((await scrypt(password, admin.salt, 64)) as Buffer).toString(
      'hex',
    )
    if (!safeEqual(hash, admin.hash))
      throw new AppError(401, 'Incorrect password.')
    return this.session()
  }

  session() {
    const value = token()
    const session = { csrf: token(), createdAt: Date.now() }
    this.store.set(
      `session:${digest(value)}`,
      session,
      Date.now() + 7 * 86400000,
    )
    return { value, ...session }
  }

  read(value: string | undefined) {
    return value
      ? this.store.kv<Session>(`session:${digest(value)}`)
      : undefined
  }

  logout(value: string) {
    this.store.delete(`session:${digest(value)}`)
  }

  register(input: Record<string, unknown>) {
    const uris = input.redirect_uris
    if (!Array.isArray(uris) || uris.length < 1 || uris.length > 10)
      throw new AppError(400, 'Provide 1–10 redirect URIs.')
    for (const uri of uris) {
      let url: URL
      try {
        url = new URL(uri as string)
      }
      catch {
        throw new AppError(400, 'Invalid redirect URI.')
      }

      if (
        url.hash
        || url.username
        || url.password
        || (url.protocol !== 'https:'
          && !(
            url.protocol === 'http:'
            && ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
          ))
      ) {
        throw new AppError(
          400,
          'Redirect URIs must use HTTPS (HTTP is allowed for loopback clients).',
        )
      }
    }

    if (this.store.keys('client:').length >= 100)
      throw new AppError(429, 'Client registration limit reached.')
    const clientId = token()
    const client = {
      client_id: clientId,
      client_name:
        typeof input.client_name === 'string'
          ? input.client_name.slice(0, 100)
          : 'MCP client',
      redirect_uris: uris as string[],
      token_endpoint_auth_method: 'none',
      grant_types: ['authorization_code', 'refresh_token'],
      response_types: ['code'],
    }
    this.store.set(`client:${clientId}`, client)
    return client
  }

  authorization(params: Record<string, string>) {
    const client = this.store.kv<{
      client_id: string
      client_name: string
      redirect_uris: string[]
    }>(`client:${params.client_id}`)
    if (!client || !client.redirect_uris.includes(params.redirect_uri))
      throw new AppError(400, 'Unknown client or redirect URI.')
    if (
      params.response_type !== 'code'
      || params.code_challenge_method !== 'S256'
      || !/^[\w-]{43}$/.test(params.code_challenge ?? '')
    ) {
      throw new AppError(
        400,
        'Authorization requires code flow with S256 PKCE.',
      )
    }

    const resource = params.resource || `${this.publicUrl}/mcp`
    if (resource !== `${this.publicUrl}/mcp`)
      throw new AppError(400, 'Resource does not match this MCP server.')
    const scopes = (params.scope || 'read').split(' ').filter(Boolean)
    if (
      !scopes.length
      || scopes.some(s => !['read', 'run', 'manage'].includes(s))
    ) {
      throw new AppError(400, 'Unsupported scope.')
    }

    return { client, resource, scopes }
  }

  consent(params: Record<string, string>, approved: boolean) {
    const { client, resource, scopes } = this.authorization(params)
    const redirect = new URL(params.redirect_uri)
    if (params.state)
      redirect.searchParams.set('state', params.state)
    if (!approved) {
      redirect.searchParams.set('error', 'access_denied')
      return redirect.href
    }

    const code = token()
    this.store.set(
      `code:${digest(code)}`,
      {
        clientId: client.client_id,
        redirectUri: params.redirect_uri,
        challenge: params.code_challenge,
        resource,
        scopes,
        label: client.client_name,
      },
      Date.now() + 300000,
    )
    redirect.searchParams.set('code', code)
    return redirect.href
  }

  issue(grant: Omit<Grant, 'expiresAt'>) {
    const access = token()
    const refresh = token()
    const expiresAt = Date.now() + 3600000
    this.store.set(
      `access:${digest(access)}`,
      { ...grant, expiresAt },
      expiresAt,
    )
    this.store.set(
      `refresh:${digest(refresh)}`,
      { ...grant, expiresAt: Date.now() + 30 * 86400000 },
      Date.now() + 30 * 86400000,
    )
    this.store.set(
      `grant:${grant.family}`,
      { ...grant, createdAt: Date.now() },
      Date.now() + 30 * 86400000,
    )
    return {
      access_token: access,
      refresh_token: refresh,
      token_type: 'Bearer',
      expires_in: 3600,
      scope: grant.scopes.join(' '),
    }
  }

  exchange(params: Record<string, string>) {
    if (params.grant_type === 'authorization_code') {
      return this.store.transaction(() => {
        const code = this.store.kv<{
          clientId: string
          redirectUri: string
          challenge: string
          resource: string
          scopes: string[]
          label: string
        }>(`code:${digest(params.code ?? '')}`)
        if (
          !code
          || params.client_id !== code.clientId
          || params.redirect_uri !== code.redirectUri
          || !params.code_verifier
          || params.code_verifier.length < 43
          || params.code_verifier.length > 128
          || digest(params.code_verifier) !== code.challenge
          || (params.resource && params.resource !== code.resource)
        ) {
          throw new OAuthError(
            'invalid_grant',
            'Invalid or expired authorization code, verifier, or resource.',
          )
        }

        this.store.delete(`code:${digest(params.code)}`)
        return this.issue({
          clientId: code.clientId,
          resource: code.resource,
          scopes: code.scopes,
          label: code.label,
          family: token(),
        })
      })
    }

    if (params.grant_type === 'refresh_token') {
      const key = `refresh:${digest(params.refresh_token ?? '')}`
      const previous = this.store.kv<Grant>(key)
      if (previous?.used) {
        this.revoke(previous.family)
        throw new OAuthError(
          'invalid_grant',
          'Refresh token reuse detected. Reconnect this client.',
        )
      }

      if (
        !previous
        || previous.clientId !== params.client_id
        || (params.resource && params.resource !== previous.resource)
      ) {
        throw new OAuthError('invalid_grant', 'Invalid refresh token.')
      }

      return this.store.transaction(() => {
        this.store.set(key, { ...previous, used: true }, previous.expiresAt)
        if (
          params.scope
          && params.scope
            .split(' ')
            .some(scope => !previous.scopes.includes(scope))
        ) {
          throw new OAuthError(
            'invalid_scope',
            'Refresh cannot add permissions.',
          )
        }

        return this.issue({
          ...previous,
          scopes: params.scope
            ? params.scope.split(' ').filter(Boolean)
            : previous.scopes,
        })
      })
    }

    throw new OAuthError('unsupported_grant_type', 'Unsupported grant type.')
  }

  verify(value: string | undefined, scope?: string) {
    const grant = value
      ? this.store.kv<Grant>(`access:${digest(value)}`)
      : undefined
    if (!grant || grant.resource !== `${this.publicUrl}/mcp`)
      throw new AppError(401, 'A valid MCP access token is required.')
    if (scope && !grant.scopes.includes(scope))
      throw new AppError(403, `The ${scope} scope is required.`)
    return grant
  }

  personal(label: string, scopes: string[]) {
    if (
      !label.trim()
      || scopes.length === 0
      || scopes.some(s => !['read', 'run', 'manage'].includes(s))
    ) {
      throw new AppError(400, 'Choose a token name and valid scopes.')
    }

    const value = token()
    const family = token()
    const grant = {
      clientId: 'personal',
      label: label.slice(0, 100),
      scopes,
      resource: `${this.publicUrl}/mcp`,
      family,
      expiresAt: Date.now() + 30 * 86400000,
    }
    this.store.set(`access:${digest(value)}`, grant, grant.expiresAt)
    this.store.set(`grant:${family}`, grant, grant.expiresAt)
    return { token: value, expiresAt: grant.expiresAt }
  }

  revokeToken(value: string, clientId: string) {
    for (const prefix of ['access:', 'refresh:']) {
      const grant = this.store.kv<Grant>(`${prefix}${digest(value)}`)
      if (grant?.clientId === clientId)
        this.revoke(grant.family)
    }
  }

  revoke(family: string) {
    for (const prefix of ['access:', 'refresh:']) {
      for (const entry of this.store.keys(prefix)) {
        if (entry.data.family === family)
          this.store.delete(entry.key)
      }
    }

    this.store.delete(`grant:${family}`)
  }
}
