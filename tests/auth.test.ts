import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { fixture } from './helpers.ts'
import { digest, safeEqual } from './legacy/server/auth.ts'

describe('administrator and OAuth credentials', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  beforeEach(async () => {
    ctx = await fixture()
  })
  afterEach(async () => {
    vi.useRealTimers()
    await ctx.dispose()
  })

  it('requires the bootstrap secret, rejects repeat setup and signs out durably', async () => {
    expect((await ctx.app.inject('/api/tasks')).statusCode).toBe(401)
    expect(
      (
        await ctx.app.inject({
          method: 'POST',
          url: '/api/setup',
          payload: { setupToken: 'wrong', password: 'password-long-enough' },
        })
      ).statusCode,
    ).toBe(403)
    const headers = await ctx.login()
    expect(
      (await ctx.app.inject({ url: '/api/tasks', headers })).statusCode,
    ).toBe(200)
    expect(
      (
        await ctx.app.inject({
          method: 'POST',
          url: '/api/setup',
          payload: {
            setupToken: 'test-setup-token',
            password: 'password-long-enough',
          },
        })
      ).statusCode,
    ).toBe(409)
    expect(
      (
        await ctx.app.inject({
          method: 'POST',
          url: '/api/logout',
          headers: { cookie: headers.cookie },
        })
      ).statusCode,
    ).toBe(403)
    expect(
      (await ctx.app.inject({ method: 'POST', url: '/api/logout', headers }))
        .statusCode,
    ).toBe(200)
    expect(
      (await ctx.app.inject({ url: '/api/tasks', headers })).statusCode,
    ).toBe(401)
    expect(
      (
        await ctx.app.inject({
          method: 'POST',
          url: '/api/login',
          payload: { password: 'wrong' },
        })
      ).statusCode,
    ).toBe(401)
    expect(
      (
        await ctx.app.inject({
          method: 'POST',
          url: '/api/login',
          payload: { password: 'test-password-long-enough' },
        })
      ).statusCode,
    ).toBe(200)
  })

  it('does not allow simultaneous setup to replace the first password', async () => {
    const results = await Promise.allSettled([
      ctx.auth.setup('first-password-long'),
      ctx.auth.setup('second-password-long'),
    ])
    expect(
      results.filter(result => result.status === 'fulfilled'),
    ).toHaveLength(1)
    expect(
      results.filter(result => result.status === 'rejected'),
    ).toHaveLength(1)
  })

  it('rejects hostile origins and hosts and compares Unicode secrets safely', async () => {
    const headers = await ctx.login()
    expect(
      (
        await ctx.app.inject({
          url: '/api/tasks',
          headers: { ...headers, origin: 'https://hostile.example' },
        })
      ).statusCode,
    ).toBe(403)
    expect(
      (
        await ctx.app.inject({
          url: '/health',
          headers: { host: 'hostile.example' },
        })
      ).statusCode,
    ).toBe(403)
    expect(safeEqual('a', 'é')).toBe(false)
    expect(safeEqual('é', 'é')).toBe(true)
    const session = ctx.auth.session()
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 8 * 86400000)
    expect(ctx.auth.read(session.value)).toBeUndefined()
  })

  function authorization() {
    const client = ctx.auth.register({
      client_name: 'Test connector',
      redirect_uris: ['https://client.example/callback'],
    })
    const verifier = 'v'.repeat(64)
    const parameters = {
      client_id: client.client_id,
      redirect_uri: client.redirect_uris[0],
      response_type: 'code',
      code_challenge: digest(verifier),
      code_challenge_method: 'S256',
      state: 'test-state',
      scope: 'read run',
      resource: 'http://localhost:4310/mcp',
    }
    const redirect = new URL(ctx.auth.consent(parameters, true))
    expect(redirect.searchParams.get('state')).toBe('test-state')
    return {
      parameters,
      exchange: {
        grant_type: 'authorization_code',
        client_id: client.client_id,
        redirect_uri: client.redirect_uris[0],
        code: redirect.searchParams.get('code')!,
        code_verifier: verifier,
        resource: parameters.resource,
      },
    }
  }

  it('binds one-use codes to client, redirect, verifier and resource', () => {
    const { exchange } = authorization()
    for (const patch of [
      { code_verifier: 'bad'.repeat(20) },
      { client_id: 'wrong' },
      { redirect_uri: 'https://client.example/elsewhere' },
      { resource: 'https://other.example/mcp' },
    ]) {
      expect(() => ctx.auth.exchange({ ...exchange, ...patch })).toThrow(
        /Invalid/,
      )
    }

    const tokens = ctx.auth.exchange(exchange)
    expect(ctx.auth.verify(tokens.access_token, 'run').scopes).toEqual([
      'read',
      'run',
    ])
    expect(() => ctx.auth.verify(tokens.access_token, 'manage')).toThrow(
      /scope/,
    )
    expect(() => ctx.auth.exchange(exchange)).toThrow(/Invalid/)
    expect(JSON.stringify(ctx.service.store.keys('access:'))).not.toContain(
      tokens.access_token,
    )
  })

  it('rotates refresh tokens and revokes the complete family on reuse', () => {
    const { exchange } = authorization()
    const initial = ctx.auth.exchange(exchange)
    const refresh = {
      grant_type: 'refresh_token',
      client_id: exchange.client_id,
      refresh_token: initial.refresh_token,
    }
    expect(() => ctx.auth.exchange({ ...refresh, scope: 'manage' })).toThrow(
      /permissions/,
    )
    const rotated = ctx.auth.exchange(refresh)
    expect(ctx.auth.verify(rotated.access_token, 'read')).toBeTruthy()
    expect(() => ctx.auth.exchange(refresh)).toThrow(/reuse/)
    expect(() => ctx.auth.verify(initial.access_token)).toThrow(/valid/)
    expect(() => ctx.auth.verify(rotated.access_token)).toThrow(/valid/)
    expect(() =>
      ctx.auth.exchange({ ...refresh, refresh_token: rotated.refresh_token }),
    ).toThrow(/Invalid/)
  })

  it('revokes with a refresh token and rejects invalid redirects and expired codes', () => {
    for (const uri of [
      'http://remote.example/cb',
      'javascript:alert(1)',
      'https://x.example/#fragment',
      'https://user:pass@x.example/cb',
    ]) {
      expect(() => ctx.auth.register({ redirect_uris: [uri] })).toThrow(
        /HTTPS/,
      )
    }

    const { exchange } = authorization()
    const tokens = ctx.auth.exchange(exchange)
    ctx.auth.revokeToken(tokens.refresh_token, exchange.client_id)
    expect(() => ctx.auth.verify(tokens.access_token)).toThrow(/valid/)
    const next = authorization()
    vi.spyOn(Date, 'now').mockReturnValue(Date.now() + 301000)
    expect(() => ctx.auth.exchange(next.exchange)).toThrow(/expired/)
  })

  it('denies consent without issuing a code and returns OAuth machine errors', async () => {
    const { parameters } = authorization()
    const denied = new URL(ctx.auth.consent(parameters, false))
    expect(denied.searchParams.get('error')).toBe('access_denied')
    expect(denied.searchParams.has('code')).toBe(false)
    const response = await ctx.app.inject({
      method: 'POST',
      url: '/oauth/token',
      payload: { grant_type: 'password' },
    })
    expect(response.statusCode).toBe(400)
    expect(response.json().error).toBe('unsupported_grant_type')
  })
})
