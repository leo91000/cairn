import { once } from 'node:events'
import { createServer, get } from 'node:http'
import { promisify } from 'node:util'
import { expect, it } from 'vitest'
import { beaconRelayFixture } from '../../tests/e2e/beacon-relay-fixture'

it('reaches the Android reverse-forwarded fixture when hostname resolution is unavailable', async () => {
  const server = createServer((_request, response) => response.end('relay ready'))
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')
  const port = (server.address() as { port: number }).port
  // The bench forwards this same device loopback port with adb reverse.
  const fixture = await beaconRelayFixture(port, '127.0.0.1', undefined, '127.0.0.1')
  let lookups = 0
  try {
    const response = await new Promise<string>((resolve, reject) => {
      get(fixture.url, {
        lookup(_hostname, _options, callback) {
          lookups++
          callback(Object.assign(new Error('No address associated with hostname'), { code: 'EAI_NODATA' }), [])
        },
      }, (response) => {
        let body = ''
        response.setEncoding('utf8')
        response.on('data', chunk => body += chunk)
        response.on('end', () => resolve(body))
        response.on('error', reject)
      }).on('error', reject)
    })
    expect(response).toBe('relay ready')
    expect(lookups).toBe(0)
  }
  finally {
    await fixture.close()
    await promisify(server.close.bind(server))()
  }
})
