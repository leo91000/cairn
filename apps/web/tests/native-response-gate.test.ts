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

async function closeOwnedServers() {
  const failures: unknown[] = []
  for (const close of cleanup.splice(0).reverse()) {
    try {
      await close()
    }
    catch (error) {
      failures.push(error)
    }
  }

  if (failures.length)
    throw new AggregateError(failures, 'Unable to close all owned HTTP servers')
}

afterEach(closeOwnedServers)

it('attempts every owned closure even when an earlier closure fails', async () => {
  const closed: string[] = []
  const failure = new Error('fixture close failure')
  cleanup.push(async () => {
    closed.push('backend')
  })
  cleanup.push(async () => {
    closed.push('gate')
    throw failure
  })
  await expect(closeOwnedServers()).rejects.toMatchObject({ errors: [failure] })
  expect(closed).toEqual(['gate', 'backend'])
  expect(cleanup).toEqual([])
})

async function fixture(availabilityHeaders = Promise.resolve()) {
  const received: Array<{
    method?: string
    cookie?: string
    origin?: string
    body: string
  }> = []
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
    if (incoming.url === '/api/installations')
      await availabilityHeaders

    response.writeHead(200, { 'content-type': 'application/json' }).end('[{"online":true}]')
  })
  backend.listen(0, '127.0.0.1')
  await once(backend, 'listening')
  cleanup.push(async () => {
    const closed = new Promise<void>(resolve => backend.close(() => resolve()))
    backend.closeAllConnections()
    await closed
  })
  const origin = `http://127.0.0.1:${(backend.address() as { port: number }).port}`
  const gate = await nativeResponseGate(origin)
  let gateClosed = false
  cleanup.push(async () => {
    if (!gateClosed)
      await gate.close()
  })

  function call(path: string, method = 'GET', body = '', target = origin) {
    const result = Promise.withResolvers<{ status?: number, body: string }>()
    let responseSeen = false
    let responseStatus: number | undefined
    let disposition: string | undefined
    const client = request(gate.server, {
      method,
      path: target + path,
      headers: { host: new URL(target).host, cookie: 'fixture-only', origin },
    }, (response) => {
      responseSeen = true
      responseStatus = response.statusCode
      disposition = response.headers['content-disposition']
      void (async () => {
        const chunks: Buffer[] = []
        for await (const chunk of response)
          chunks.push(chunk)
        return { status: response.statusCode, body: Buffer.concat(chunks).toString() }
      })().then(result.resolve, result.reject)
    })
    client.on('error', result.reject)
    client.end(body)
    return {
      client,
      result: result.promise,
      responseSeen: () => responseSeen,
      responseStatus: () => responseStatus,
      disposition: () => disposition,
    }
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
  expect(held.headersDelivered).toBe(true)
  gate.check()
})

it('delivers genuine headers while holding the native response body', async () => {
  const { gate, call } = await fixture()
  const held = gate.holdBody()
  const read = call('/api/installations')
  let delivered = false
  void read.result.then(() => delivered = true, () => delivered = true)
  await vi.waitFor(() => expect(read.responseSeen()).toBe(true))
  expect(read.responseStatus()).toBe(200)
  expect(held.ready).toBe(true)
  expect(held.received).toBe(true)
  expect(held.status).toBe(200)
  expect(delivered).toBe(false)
  held.release()
  expect(await read.result).toEqual({ status: 200, body: '[{"online":true}]' })
  await vi.waitFor(() => expect(held.closed).toBe(true))
  expect(held.terminal).toBe('finished')
  gate.check()
})

it('advances only genuine headers while retaining the body until caller cancellation', async () => {
  const { gate, call } = await fixture()
  const held = gate.hold('availability')
  const read = call('/api/installations')
  const rejected = expect(read.result).rejects.toThrow()
  await vi.waitFor(() => expect(held.ready).toBe(true))
  expect(read.responseSeen()).toBe(false)
  held.releaseHeaders()
  await vi.waitFor(() => expect(read.responseSeen()).toBe(true))
  expect(read.responseStatus()).toBe(200)
  expect(held.status).toBe(200)
  expect(held.headersDelivered).toBe(true)
  expect(held.bodyReleased).toBe(false)
  read.client.destroy(new Error('Caller withdrawn after headers'))
  await rejected
  await vi.waitFor(() => expect(held.closed).toBe(true))
  expect(held.terminal).toBe('cancelled')
  expect(held.bodyReleased).toBe(false)
  gate.check()
})

it('holds the real inert health destination until the old document has retired its read', async () => {
  const { gate, call } = await fixture()
  const held = gate.hold('health')
  const read = call('/health')
  await vi.waitFor(() => expect(held.ready).toBe(true))
  expect(read.responseSeen()).toBe(false)
  expect(() => held.releaseHeaders()).toThrow('Only an unchanged availability response')
  held.release()
  expect(await read.result).toEqual({ status: 200, body: '[{"online":true}]' })
  await vi.waitFor(() => expect(held.closed).toBe(true))
  expect(held.terminal).toBe('finished')
  expect(held.headersDelivered).toBe(true)
  gate.check()
})

it('keeps a controlled readonly outage active across HTTP attempts and restores the origin', async () => {
  const { gate, call, received } = await fixture()
  const outage = gate.failAvailabilityReads()
  for (let attempt = 0; attempt < 2; attempt++) {
    const read = call('/api/installations')
    await expect(read.result).rejects.toThrow()
    expect(read.responseSeen()).toBe(false)
  }

  expect(outage.rejectedRequests).toBe(2)
  expect(await call('/health').result).toEqual({ status: 200, body: '[{"online":true}]' })
  outage.restore()
  expect(await call('/api/installations').result).toEqual({ status: 200, body: '[{"online":true}]' })
  expect(gate.availabilityRequests).toBe(3)
  expect(received).toHaveLength(2)
  gate.check()
})

it('produces a real read failure before headers and during an already started body', async () => {
  for (const bodyStarted of [false, true]) {
    const { gate, call } = await fixture()
    const held = bodyStarted ? gate.holdBody() : gate.hold('availability')
    const pending = call('/api/installations')
    const rejected = expect(pending.result).rejects.toThrow()
    await vi.waitFor(() => expect(held.ready).toBe(true))
    if (bodyStarted)
      await vi.waitFor(() => expect(pending.responseSeen()).toBe(true))
    held.disconnect()
    await rejected
    await vi.waitFor(() => expect(held.closed).toBe(true))
    expect(held.terminal).toBe('cancelled')
    gate.check()
  }
})

it('records HTTP ingress before upstream headers even when the caller cancels', async () => {
  const headers = Promise.withResolvers<void>()
  const { gate, call } = await fixture(headers.promise)
  const held = gate.hold('availability')
  const read = call('/api/installations')
  const rejected = expect(read.result).rejects.toThrow('owned cancellation before headers')
  try {
    await vi.waitFor(() => expect(held.received).toBe(true))
    expect(held.ready).toBe(false)
    expect(held.status).toBeUndefined()
    read.client.destroy(new Error('owned cancellation before headers'))
    await rejected
    await vi.waitFor(() => expect(held.closed).toBe(true))
    expect(held.terminal).toBe('cancelled')
    expect(held.ready).toBe(false)
    expect(held.received).toBe(true)
    gate.check()
  }
  finally {
    read.client.destroy(new Error('owned cancellation before headers'))
    headers.resolve()
    held.release()
    await rejected
  }
})

it('preserves account context and forwards a mutation exactly once', async () => {
  const {
    gate,
    origin,
    received,
    call,
  } = await fixture()
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

it('turns the destination response into a download without destroying its HTTP connection', async () => {
  const { gate, call } = await fixture()
  const destination = gate.hold('agents')
  const navigation = call('/installations/fixture/agents')
  await vi.waitFor(() => expect(destination.ready).toBe(true))
  destination.downloadNavigation()
  expect(await navigation.result).toEqual({ status: 200, body: '[{"online":true}]' })
  expect(navigation.disposition()).toBe('attachment; filename="availability-navigation.html"')
  await vi.waitFor(() => expect(destination.closed).toBe(true))
  expect(destination.terminal).toBe('finished')
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
  expect(held.closed).toBe(true)
  await gate.close()
})

it('refuses another origin before forwarding any account credentials', async () => {
  const {
    gate,
    received,
    call,
    closeExpectedFailure,
  } = await fixture()
  expect((await call('/api/installations', 'GET', '', 'http://unrelated.invalid').result).status).toBe(502)
  expect(received).toEqual([])
  expect(() => gate.check()).toThrow('The native response gate failed')
  await closeExpectedFailure()
})
