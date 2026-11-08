import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { connect } from 'node:net'
import { expect, it } from 'vitest'
import { mcpProvider } from '../../tests/fixtures/mcp-provider'

it('closes the provider even when a client keeps an HTTP connection open', async () => {
  const provider = await mcpProvider()
  const socket = connect(Number(new URL(provider.origin).port), '127.0.0.1')
  let closing: Promise<void> | undefined
  let deadline: ReturnType<typeof setTimeout> | undefined
  socket.on('error', () => { /* Server shutdown may reset an incomplete request. */ })
  try {
    await once(socket, 'connect')
    // Leave an incomplete request in flight, as a streaming client can do.
    socket.write('GET /mcp HTTP/1.1\r\nHost: localhost\r\n')
    const closed = new Promise<void>((resolve, reject) => {
      deadline = setTimeout(() => reject(new Error('Provider left its client connection open')), 1500)
      socket.once('close', () => resolve())
    })
    closing = provider.close()
    await closed
    await closing
    expect(socket.destroyed).toBe(true)
  }
  finally {
    clearTimeout(deadline)
    socket.destroy()
    await (closing || provider.close())
  }
})

it.each(['http-redirect', 'document'] as const)('keeps the real OAuth callback and PKCE exchange with %s navigation', async (authorizationNavigation) => {
  const provider = await mcpProvider({ authorizationNavigation })
  try {
    const verifier = 'fixture-verifier'
    const state = 'state with </script> and "quotes"'
    const authorize = new URL('/authorize', provider.origin)
    authorize.searchParams.set('redirect_uri', 'http://localhost/callback?existing=preserved')
    authorize.searchParams.set('state', state)
    authorize.searchParams.set('code_challenge', createHash('sha256').update(verifier).digest('base64url'))
    const response = await fetch(authorize, { redirect: 'manual' })
    let callback: URL
    if (authorizationNavigation === 'http-redirect') {
      expect(response.status).toBe(302)
      callback = new URL(response.headers.get('location')!)
    }
    else {
      expect(response.status).toBe(200)
      expect(response.headers.get('content-type')).toContain('text/html')
      expect(response.headers.has('location')).toBe(false)
      const html = await response.text()
      const target = html.match(/window\.location\.replace\((.+)\)/)?.[1]
      expect(target).toBeDefined()
      expect(target).not.toContain('<')
      callback = new URL(JSON.parse(target!))
    }

    expect(callback.origin).toBe('http://localhost')
    expect(callback.pathname).toBe('/callback')
    expect(callback.searchParams.get('existing')).toBe('preserved')
    expect(callback.searchParams.get('state')).toBe(state)
    expect(callback.searchParams.get('iss')).toBe(provider.origin)
    expect(callback.searchParams.get('code')).toBe('fixture-code')
    const exchange = await fetch(new URL('/token', provider.origin), {
      method: 'POST',
      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
      body: new URLSearchParams({ code: callback.searchParams.get('code')!, code_verifier: verifier }),
    })
    expect(exchange.status).toBe(200)
    expect(provider.exchanges).toBe(1)
  }
  finally {
    await provider.close()
  }
})
