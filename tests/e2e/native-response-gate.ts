import type { ClientRequest } from 'node:http'
import { once } from 'node:events'
import { Agent, createServer, request as httpRequest } from 'node:http'

export type ResponseKind = 'availability' | 'agents'

export interface HeldResponse {
  ready: boolean
  status?: number
  terminal: 'pending' | 'finished' | 'cancelled' | 'failed'
  release: () => void
  cancelNavigation: () => void
}

// A real HTTP proxy for only one disposable fixture origin. Holding bytes here
// leaves the browser's native request intact; no Playwright routing is enabled.
export async function nativeResponseGate(origin: string) {
  const upstreamOrigin = new URL(origin)
  if (upstreamOrigin.protocol !== 'http:' || !['localhost', '127.0.0.1'].includes(upstreamOrigin.hostname)
    || upstreamOrigin.username || upstreamOrigin.password || !upstreamOrigin.port) {
    throw new Error('The response gate requires one explicit loopback HTTP origin')
  }

  const agent = new Agent({ keepAlive: false })
  const next = new Map<ResponseKind, ReturnType<typeof control>>()
  const controls = new Set<ReturnType<typeof control>>()
  const requests = new Set<ClientRequest>()
  const requestClosures = new Set<Promise<void>>()
  const responseClosures = new Set<Promise<void>>()
  const deliveries = new Set<Promise<void>>()
  const failures: unknown[] = []
  let closing = false
  let closeResult: Promise<void> | undefined

  function control(kind: ResponseKind, unavailable = false) {
    const released = Promise.withResolvers<void>()
    let cancel = false
    const response: HeldResponse = {
      ready: false,
      terminal: 'pending',
      release: () => released.resolve(),
      cancelNavigation: () => {
        if (kind !== 'agents')
          throw new Error('Only the destination navigation may be deliberately cancelled')

        cancel = true
        released.resolve()
      },
    }
    return {
      response,
      released: released.promise,
      cancelled: () => cancel,
      unavailable,
    }
  }

  const server = createServer((request, response) => {
    const responseClosed = new Promise<void>(resolve => response.once('close', () => resolve()))
    responseClosures.add(responseClosed)
    void responseClosed.then(() => responseClosures.delete(responseClosed))
    let target: URL
    try {
      target = new URL(request.url || '/', upstreamOrigin)
    }
    catch {
      failures.push(new Error('Invalid response-gate request target'))
      response.writeHead(502).end()
      return
    }

    if (closing || target.origin !== upstreamOrigin.origin || target.username || target.password) {
      failures.push(new Error('Unexpected response-gate origin or request after closure'))
      response.writeHead(502).end()
      return
    }

    const kind = request.method === 'GET' && target.pathname === '/api/installations'
      ? 'availability'
      : request.method === 'GET' && /^\/installations\/[\w-]+\/agents$/.test(target.pathname) ? 'agents' : undefined
    const held = kind ? next.get(kind) : undefined
    if (kind && held)
      next.delete(kind)

    const headers = { ...request.headers }
    delete headers['proxy-connection']
    const upstream = httpRequest({
      hostname: '127.0.0.1',
      port: Number(upstreamOrigin.port),
      method: request.method,
      path: target.pathname + target.search,
      headers,
      agent,
    }, (source) => {
      if (held) {
        source.pause()
        held.response.status = source.statusCode
        held.response.ready = true
      }

      const delivery = (async () => {
        if (held)
          await held.released

        if (closing || response.destroyed || held?.cancelled()) {
          source.destroy()
          response.destroy()
          return
        }

        if (held?.unavailable) {
          source.resume()
          response.writeHead(503, { 'content-type': 'application/json' }).end('{"error":"Installation status unavailable"}')
          return
        }

        response.writeHead(source.statusCode || 502, source.headers)
        source.pipe(response)
      })()
      deliveries.add(delivery)
      void delivery.then(() => deliveries.delete(delivery), (error) => {
        failures.push(error)
        deliveries.delete(delivery)
        source.destroy()
        response.destroy()
      })
      source.on('error', (error) => {
        if (!closing && !response.destroyed)
          failures.push(error)
        response.destroy()
      })
    })
    requests.add(upstream)
    const closed = new Promise<void>((resolve) => {
      upstream.once('close', () => {
        requests.delete(upstream)
        resolve()
      })
    })
    requestClosures.add(closed)
    void closed.then(() => requestClosures.delete(closed))
    response.on('finish', () => {
      if (held)
        held.response.terminal = 'finished'
    })
    response.on('close', () => {
      if (held && !response.writableFinished)
        held.response.terminal = 'cancelled'
      upstream.destroy()
    })
    request.on('aborted', () => upstream.destroy())
    upstream.on('error', (error) => {
      if (!closing && !response.destroyed) {
        failures.push(error)
        if (held)
          held.response.terminal = 'failed'
      }

      response.destroy()
    })
    request.pipe(upstream)
  })
  server.on('connect', (_request, socket) => {
    failures.push(new Error('The HTTP fixture gate does not admit CONNECT'))
    socket.destroy()
  })
  server.on('upgrade', (_request, socket) => {
    failures.push(new Error('The HTTP fixture gate does not admit upgrades'))
    socket.destroy()
  })
  server.listen(0, '127.0.0.1')
  try {
    await once(server, 'listening')
  }
  catch (error) {
    agent.destroy()
    server.close()
    throw error
  }

  function hold(kind: ResponseKind, unavailable = false) {
    if (closing || next.has(kind))
      throw new Error('A response gate must be closed before arming another')
    const held = control(kind, unavailable)
    next.set(kind, held)
    controls.add(held)
    return held.response
  }

  function check() {
    if (failures.length)
      throw new AggregateError(failures, 'The native response gate failed')
  }

  return {
    server: `http://127.0.0.1:${(server.address() as { port: number }).port}`,
    hold: (kind: ResponseKind) => hold(kind),
    unavailable: () => {
      const held = hold('availability', true)
      held.release()
      return held
    },
    check,
    close() {
      closeResult ??= (async () => {
        closing = true
        for (const held of controls)
          held.response.release()
        const stopped = new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
        server.closeAllConnections()
        for (const request of requests)
          request.destroy()
        agent.destroy()
        await stopped
        await Promise.all([...requestClosures, ...responseClosures])
        await Promise.all(deliveries)
        check()
      })()

      return closeResult
    },
  }
}
