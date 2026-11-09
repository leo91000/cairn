import { Buffer } from 'node:buffer'
import { once } from 'node:events'
import { createServer, request } from 'node:http'
import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import { nativeResponseGate } from '../../../tests/e2e/native-response-gate'

const cleanup: Array<() => Promise<void>> = []

afterEach(async () => {
  for (const close of cleanup.splice(0).reverse())
    await close()
})

async function fixture() {
  const received: Array<{ method?: string, cookie?: string, origin?: string, body: string }> = []
  const backend = createServer(async (incoming, response) => {
    const chunks: Buffer[] = []
    for await (const chunk of incoming)
      chunks.push(chunk)
    received.push({
      method: incoming.method,
      cookie: incoming.headers.cookie,
      origin: incoming.headers.origin,
      body: Buffer.concat(chunks).toString(),
    })
    response.writeHead(200, { 'content-type': 'application/json' }).end('[{"online":true}]')
  })
  backend.listen(0, '127.0.0.1')
  await once(backend, 'listening')
  cleanup.push(async () => {
    const closed = new Promise<void>(resolve => backend.close(() => resolve()))
    backend.closeAllConnections()
    await closed
  })
  const origin = 'http://127.0.0.1:' + (backend.address() as { port: number }).port
  const gate = await nativeResponseGate(origin)
  let gateClosed = false
  cleanup.push(async () => {
    if (!gateClosed)
      await gate.close()
  })

  function call(path: string, method = 'GET', body = '', target = origin) {
    const result = Promise.withResolvers<{ status?: number, body: string }>()
    let responseSeen = false
    const client = request(gate.server, {
      method,
      path: target + path,
      headers: { host: new URL(target).host, cookie: 'fixture-only', origin },
    }, (response) => {
      responseSeen = true
      void (async () => {
        const chunks: Buffer[] = []
        for await (const chunk of response)
          chunks.push(chunk)
        return { status: response.statusCode, body: Buffer.concat(chunks).toString() }
      })().then(result.resolve, result.reject)
    })
    client.on('error', result.reject)
    client.end(body)
    return { client, result: result.promise, responseSeen: () => responseSeen }
  }

  return {
    gate,
    origin,
    received,
    call,
    async closeExpectedFailure() {
      await expect(gate.close()).rejects.toThrow('The native response gate failed')
      gateClosed = true
    },
  }
}

it('holds real HTTP headers until release and observes complete delivery', async () => {
  const { gate, call } = await fixture()
  const held = gate.hold('availability')
  const read = call('/api/installations')
  await vi.waitFor(() => expect(held.ready).toBe(true))
  expect(held.status).toBe(200)
  expect(read.responseSeen()).toBe(false)
  held.release()
  expect(await read.result).toEqual({ status: 200, body: '[{"online":true}]' })
  await vi.waitFor(() => expect(held.terminal).toBe('finished'))
  gate.check()
})

it('preserves account context and forwards a mutation exactly once', async () => {
  const { gate, origin, received, call } = await fixture()
  expect((await call('/api/fixture', 'POST', 'preserved').result).status).toBe(200)
  expect(received).toEqual([{
    method: 'POST',
    cookie: 'fixture-only',
    origin,
    body: 'preserved',
  }])
  gate.check()
})

it('observes a caller closing its native connection while the response is held', async () => {
  const { gate, call } = await fixture()
  const held = gate.hold('availability')
  const read = call('/api/installations')
  const rejected = expect(read.result).rejects.toThrow('owned cancellation')
  await vi.waitFor(() => expect(held.ready).toBe(true))
  read.client.destroy(new Error('owned cancellation'))
  await rejected
  await vi.waitFor(() => expect(held.terminal).toBe('cancelled'))
  held.release()
  gate.check()
})

it('sends a genuine controlled HTTP503 after a successful upstream read', async () => {
  const { gate, received, call } = await fixture()
  const held = gate.unavailable()
  expect((await call('/api/installations').result).status).toBe(503)
  expect(held.status).toBe(200)
  expect(received).toHaveLength(1)
  gate.check()
})

it('cancels the destination HTTP navigation without cancelling the availability control', async () => {
  const { gate, call } = await fixture()
  const destination = gate.hold('agents')
  const navigation = call('/installations/fixture/agents')
  const rejected = expect(navigation.result).rejects.toThrow()
  await vi.waitFor(() => expect(destination.ready).toBe(true))
  destination.cancelNavigation()
  await rejected
  await vi.waitFor(() => expect(destination.terminal).toBe('cancelled'))
  gate.check()
})

it('closes owned pending responses and is safe to close twice', async () => {
  const { gate, call } = await fixture()
  const held = gate.hold('availability')
  const read = call('/api/installations')
  const rejected = expect(read.result).rejects.toThrow()
  await vi.waitFor(() => expect(held.ready).toBe(true))
  await gate.close()
  await rejected
  expect(held.terminal).toBe('cancelled')
  await gate.close()
})

it('refuses another origin before forwarding any account credentials', async () => {
  const { gate, received, call, closeExpectedFailure } = await fixture()
  expect((await call('/api/installations', 'GET', '', 'http://unrelated.invalid').result).status).toBe(502)
  expect(received).toEqual([])
  expect(() => gate.check()).toThrow('The native response gate failed')
  await closeExpectedFailure()
})
