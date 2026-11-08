import type { Page, Request, Response } from '@playwright/test'
import { setImmediate } from 'node:timers/promises'
import { expect, it, vi } from 'vitest'
import { revokeMcpClient } from '../../tests/e2e/mcp-revocation'

it('does not probe revoked access while the clicked DELETE is still pending', async () => {
  const deletion = Promise.withResolvers<Response | null>()
  const requested = Promise.withResolvers<Request>()
  const responseFinished = Promise.withResolvers<Error | null>()
  const origin = 'http://localhost:4396'
  const url = `${origin}/api/installations/installation/tokens/oauth-grant`
  const request = { method: () => 'DELETE', url: () => url, response: () => deletion.promise } as Request
  let revoked = false
  const waitForRequest = vi.fn<Page['waitForRequest']>(async (predicate) => {
    const observed = await requested.promise
    expect(typeof predicate).toBe('function')
    const matches = predicate as (request: Request) => boolean
    expect(matches({ method: () => 'GET', url: () => url } as Request)).toBe(false)
    expect(matches({ method: () => 'DELETE', url: () => url.replace(origin, 'http://other.test') } as Request)).toBe(false)
    expect(matches({ method: () => 'DELETE', url: () => `${url}/other` } as Request)).toBe(false)
    expect(matches(observed)).toBe(true)
    return observed
  })
  const clickRevoke = vi.fn(async () => {
    requested.resolve(request)
  })
  const probe = vi.fn(() => revoked ? 401 : 200)
  const checkRevokedAccess = (async () => {
    await revokeMcpClient({ waitForRequest }, clickRevoke, origin)
    return probe()
  })()

  try {
    await setImmediate()
    expect(clickRevoke).toHaveBeenCalledOnce()
    expect(probe).not.toHaveBeenCalled()

    revoked = true
    deletion.resolve({ status: () => 200, finished: () => responseFinished.promise } as Response)
    await setImmediate()
    expect(probe).not.toHaveBeenCalled()

    responseFinished.resolve(null)
    expect(await checkRevokedAccess).toBe(401)
    expect(probe).toHaveBeenCalledOnce()
  }
  finally {
    deletion.resolve({ status: () => 200, finished: () => responseFinished.promise } as Response)
    responseFinished.resolve(null)
    await checkRevokedAccess
  }
})
