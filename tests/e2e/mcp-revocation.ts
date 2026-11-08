import type { Page } from '@playwright/test'
import assert from 'node:assert/strict'

export async function revokeMcpClient(page: Pick<Page, 'waitForRequest'>, clickRevoke: () => Promise<unknown>, beaconOrigin: string) {
  const expectedOrigin = new URL(beaconOrigin).origin
  const requested = page.waitForRequest((request) => {
    const destination = new URL(request.url())
    return request.method() === 'DELETE'
      && destination.origin === expectedOrigin
      && /^\/api\/installations\/[^/]+\/tokens\/[^/]+$/.test(destination.pathname)
  })

  // A delivered click does not await the view's asynchronous revocation.
  await clickRevoke()
  const request = await requested
  const response = await request.response()
  assert.ok(response, 'The clicked MCP revocation must receive a response')
  assert.equal(response.status(), 200, 'The clicked MCP revocation must succeed')
  assert.equal(await response.finished(), null, 'The clicked MCP revocation response must finish')
}
