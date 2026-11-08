import type { Page, Request, Response } from '@playwright/test'
import { execFileSync } from 'node:child_process'
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

it.each(['click rejects', 'click throws', 'wait rejects'])('observes both operations when %s', (mode) => {
  const helper = new URL('../../tests/e2e/mcp-revocation.ts', import.meta.url).href
  const script = `
import { setImmediate } from 'node:timers/promises';
import { revokeMcpClient } from ${JSON.stringify(helper)};
const mode = ${JSON.stringify(mode)};
const requested = Promise.withResolvers();
const clicked = Promise.withResolvers();
const unhandled = [];
process.on('unhandledRejection', error => unhandled.push(error.message));
const click = () => {
  if (mode === 'click throws') throw new Error('click failed');
  if (mode === 'click rejects') return Promise.reject(new Error('click failed'));
  return clicked.promise;
};
const outcome = revokeMcpClient({waitForRequest: () => requested.promise}, click, 'http://localhost:4396')
  .then(() => 'unexpected success', error => error.message);
if (mode === 'wait rejects') {
  requested.reject(new Error('wait failed'));
  await setImmediate();
  clicked.resolve();
} else {
  await outcome;
  requested.reject(new Error('wait failed after click'));
}
const result = await outcome;
await setImmediate();
await setImmediate();
process.stdout.write(JSON.stringify({result, unhandled}));
`
  const output = execFileSync(process.execPath, ['--input-type=module', '-e', script], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
    timeout: 10000,
  })
  expect(JSON.parse(output)).toEqual({
    result: mode === 'wait rejects' ? 'wait failed' : 'click failed',
    unhandled: [],
  })
})
