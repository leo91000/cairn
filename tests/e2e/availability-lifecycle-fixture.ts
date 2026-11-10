import type { AddressInfo } from 'node:net'
import { once } from 'node:events'
import { lstat, readFile, realpath } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'

export function lifecycleAvailability(online: boolean) {
  return [{ id: 'lifecycle-installation', online, updateRequired: false }]
}

// Existing user preference, not a disabled monitor or a replaced uploader. Keep
// unrelated telemetry out of this fixed-data fixture even on a slow native run.
export function lifecycleStorageState(origin: string) {
  return {
    cookies: [],
    origins: [{ origin, localStorage: [{ name: 'cairn_performance_upload', value: 'false' }] }],
  }
}

// Synthetic, non-sensitive data only. This cache policy is NOT a Beacon override.
// The shipped Beacon entry loads /claim without selecting an installation, so the
// real InstallationTransport.start() guard refuses RTC without replacing it.
export async function availabilityLifecycleFixture(directory = path.resolve('dist')) {
  const root = await realpath(directory)
  const failures: unknown[] = []
  const reads = new Set<Promise<void>>()
  let online = true
  let closing = false
  let closeResult: Promise<void> | undefined
  let availabilityRequests = 0
  const installation = {
    id: 'lifecycle-installation',
    name: 'Lifecycle installation',
    role: 'owner',
    online: true,
    updateRequired: false,
  }
  const types: Record<string, string> = {
    '.html': 'text/html',
    '.js': 'text/javascript',
    '.css': 'text/css',
    '.json': 'application/json',
    '.webmanifest': 'application/manifest+json',
    '.svg': 'image/svg+xml',
    '.png': 'image/png',
    '.woff2': 'font/woff2',
    '.ico': 'image/x-icon',
  }
  const server = createServer((request, response) => {
    const operation = (async () => {
      if (closing)
        throw new Error('Lifecycle request admitted after retirement')
      const target = new URL(request.url || '/', 'http://localhost')
      if (request.method !== 'GET' || target.search || target.origin !== 'http://localhost')
        throw new Error('Unexpected lifecycle fixture request')
      const pathname = target.pathname
      response.setHeader('Cache-Control', 'no-cache')
      const json = (value: unknown) => {
        response.writeHead(200, { 'Content-Type': 'application/json' })
        response.end(JSON.stringify(value))
      }

      if (pathname === '/api/account/session') {
        json({
          authenticated: true,
          account: { id: 'lifecycle-account', email: 'lifecycle@example.test' },
          csrf: 'synthetic-only',
          installations: [installation],
        })
        return
      }

      if (pathname === '/api/account/options') {
        json({ google: false, github: false, passkeys: false })
        return
      }

      if (pathname === '/api/account/invitations') {
        json([])
        return
      }

      if (pathname === '/api/installations') {
        availabilityRequests++
        json(lifecycleAvailability(online))
        return
      }

      if (pathname === '/inert' || pathname === '/health') {
        response.writeHead(200, { 'Content-Type': 'text/html' })
        response.end(`<!doctype html><title>Lifecycle control</title><h1>${pathname === '/inert' ? 'Inert control' : 'Away'}</h1>`)
        return
      }

      // Exact built entry, unchanged scripts and assets. No path fallback to a
      // successful page: unexpected APIs, traversals and missing assets are red.
      const relative = pathname === '/claim' ? 'beacon.html' : pathname.slice(1)
      if (pathname !== '/claim' && !(/^\/assets\/[\w.-]+$/.test(pathname)
        || ['/web-build.json', '/theme.js', '/favicon.svg', '/manifest.webmanifest'].includes(pathname)
        || /^\/(?:icons|brand)\/[\w.-]+$/.test(pathname))) {
        throw new Error('Unexpected lifecycle fixture path')
      }

      const file = path.join(root, relative)
      const resolved = await realpath(file)
      if (!resolved.startsWith(`${root}${path.sep}`) || !(await lstat(file)).isFile())
        throw new Error('Lifecycle asset escaped its immutable build')
      const type = types[path.extname(file)]
      if (!type)
        throw new Error('Unsupported lifecycle asset type')
      response.writeHead(200, { 'Content-Type': type })
      response.end(await readFile(file))
    })()
    reads.add(operation)
    void operation.catch((cause) => {
      failures.push(cause)
      response.destroy()
    }).finally(() => reads.delete(operation))
  })
  server.listen(0, '127.0.0.1')
  await once(server, 'listening')

  function check() {
    if (failures.length)
      throw new AggregateError(failures, 'Lifecycle HTTP fixture failed')
  }

  return {
    url: `http://localhost:${(server.address() as AddressInfo).port}`,
    get availabilityRequests() { return availabilityRequests },
    setAvailability(value: boolean) {
      if (closing)
        throw new Error('Cannot update a retired lifecycle fixture')
      online = value
    },
    check,
    close() {
      closeResult ??= (async () => {
        closing = true
        const stopped = new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()))
        server.closeAllConnections()
        await stopped
        await Promise.allSettled([...reads])
        check()
      })()
      return closeResult
    },
  }
}

export async function lifecycleBound<T>(operation: () => Promise<T>, label: string) {
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      Promise.resolve().then(operation),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`Lifecycle deadline: ${label}`)), 7000)
      }),
    ])
  }
  finally {
    clearTimeout(timer)
  }
}

// Used by the real case and light controller tests. A resolved close without an
// affirmative receipt is unknown; no failed owner prevents another from closing.
export async function retireLifecycleOwners(owners: Array<() => Promise<boolean>>) {
  const results = await Promise.allSettled(owners.map((close, index) => lifecycleBound(async () => {
    if (await close() !== true)
      throw new Error('Lifecycle closure is unknown')
  }, `owner ${index}`)))
  return results.flatMap(result => result.status === 'rejected' ? [result.reason] : [])
}
