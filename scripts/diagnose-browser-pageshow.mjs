import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  mkdir,
  readFile,
  symlink,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as delay } from 'node:timers/promises'
import { webkit } from '@playwright/test'
import { createServer } from 'vite'

const sources = [
  ['reference', 'c186d11e048cdcb8d2d1ae31a11a277a03411baf'],
  ['candidate', '2c1f472c93241150f53cf32ae1862cedf9e9a3cc'],
]
const root = process.cwd()
const output = path.join(root, 'browser-pageshow-evidence')
await mkdir(output, { recursive: true })
const git = (args, cwd = root) => execFileSync('git', ['-c', `safe.directory=${cwd}`, ...args], { cwd, encoding: 'utf8' }).trim()
const hash = value => createHash('sha256').update(value).digest('hex')
const observer = git(['rev-parse', 'HEAD'])
const installed = JSON.parse(await readFile('node_modules/@playwright/test/package.json', 'utf8')).version
const preinstalled = JSON.parse(await readFile('/ms-playwright/.docker-info', 'utf8')).driverVersion
assert.equal(installed, '1.64.1')
assert.equal(installed, preinstalled)
await writeFile(path.join(output, 'identity.json'), `${JSON.stringify({
  observer,
  sources,
  playwright: installed,
  diagnosticOnly: true,
  nativeBuild: false,
}, null, 2)}\n`)
const browser = await webkit.launch()
const results = []

async function observe(label, sha) {
  const directory = path.join(root, `pageshow-${label}`)
  git(['worktree', 'add', '--detach', directory, sha])
  assert.equal(git(['rev-parse', 'HEAD'], directory), sha)
  for (const file of ['package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml'])
    assert.deepEqual(await readFile(path.join(directory, file)), await readFile(path.join(root, file)))
  await symlink(path.join(root, 'node_modules'), path.join(directory, 'node_modules'))

  const events = []
  const pageerrors = []
  const sourceHashes = {}
  const installationId = '00000000-0000-4000-8000-000000000001'
  const slug = 'quiet-cedar-1234abcd'
  const originalPath = `/installations/${installationId}/mcps/callback`
  const canonicalPath = `/installations/${slug}/mcps/callback`
  const redirected = Promise.withResolvers()
  let barrierResponse
  let canonicalResponse
  let origin = ''
  const started = performance.now()
  const record = value => events.push({ elapsed: performance.now() - started, ...value })
  const session = {
    authenticated: true,
    account: { id: 'diagnostic-account', email: 'fixture@example.test' },
    csrf: 'diagnostic-csrf',
    installations: [{
      id: installationId,
      slug,
      name: 'Diagnostic',
      role: 'owner',
      online: true,
      updateRequired: false,
    }],
  }

  const server = await createServer({
    configFile: path.join(directory, 'apps/web/vite.config.ts'),
    cacheDir: path.join(root, 'pageshow-vite-cache'),
    server: { host: '127.0.0.1', port: 0, fs: { allow: [root] } },
    plugins: [{
      name: 'insertion-only-lifecycle-observer',
      enforce: 'pre',
      async transform(code, id) {
        const file = id.split('?')[0]
        let observed = code
        if (file === path.join(directory, 'apps/web/src/api.ts')) {
          const marker = '    window.location.assign(url)'
          assert.equal(code.split(marker).length, 2)
          observed = code.replace(marker, `    window.__pageshowDiagnosticRecord({ kind: 'redirect', redirecting: state.redirecting, aborted: documentLifetime.value.signal.aborted })\n${marker}`)
          observed += '\nwindow.__pageshowDiagnosticState = state\n'
        }
        else if (file === path.join(directory, 'apps/web/src/document-lifetime.ts')) {
          observed += '\nwindow.__pageshowDiagnosticLifetime = () => documentLifetime.value.signal.aborted\n'
        }
        else {
          return
        }

        assert.equal(code, await readFile(file, 'utf8'))
        sourceHashes[path.relative(directory, file)] = { original: hash(code), observed: hash(observed) }
        return { code: observed, map: null }
      },
      configureServer(vite) {
        vite.middlewares.use(async (request, response, next) => {
          const pathname = new URL(request.url, 'http://fixture.invalid').pathname
          if (pathname === originalPath) {
            const html = await readFile(path.join(directory, 'apps/web/beacon.html'), 'utf8')
            const observed = html.replace('</body>', '<img alt="" src="/__diagnostic/load-barrier.svg"></body>')
            response.setHeader('content-type', 'text/html')
            response.end(await vite.transformIndexHtml(originalPath, observed))
          }
          else if (pathname === canonicalPath) {
            record({ kind: 'canonical-navigation-request' })
            canonicalResponse = response
            redirected.resolve()
          }
          else if (pathname === '/__diagnostic/load-barrier.svg') {
            record({ kind: 'load-barrier-request' })
            barrierResponse = response
          }
          else if (pathname.startsWith('/api/')) {
            const csrfValid = request.headers['x-csrf-token'] === 'diagnostic-csrf'
            const originValid = request.headers.origin === origin
            let status = 200
            let value
            if (pathname === '/api/account/session') {
              value = session
            }
            else if (pathname === '/api/account/options') {
              value = { google: false, github: false, passkeys: false }
            }
            else if (pathname === '/api/installations') {
              value = session.installations
            }
            else if (pathname.endsWith('/direct/authorize')) {
              status = csrfValid && originValid ? 200 : 403
              value = status === 200 ? { available: false } : { error: 'Fixture authorization headers refused' }
            }
            else {
              status = 500
              value = { error: 'Unexpected diagnostic HTTP route' }
            }

            record({
              kind: 'http',
              pathname,
              method: request.method,
              status,
              csrfValid,
              originValid,
            })
            response.writeHead(status, { 'content-type': 'application/json' }).end(JSON.stringify(value))
          }
          else {
            next()
          }
        })
      },
    }],
  })
  const context = await browser.newContext({ isMobile: true, hasTouch: true })
  await context.tracing.start({ screenshots: true, snapshots: true, sources: true })
  const page = await context.newPage()
  page.on('pageerror', (error) => {
    pageerrors.push({ message: error.message, name: error.name, stack: error.stack })
    record({ kind: 'pageerror', message: error.message })
  })
  page.on('requestfailed', request => record({ kind: 'requestfailed', pathname: new URL(request.url()).pathname, failure: request.failure()?.errorText }))
  await page.exposeFunction('__pageshowDiagnosticRecord', record)
  await page.addInitScript(() => {
    const deliver = window.__pageshowDiagnosticRecord
    const documentId = crypto.randomUUID()
    window.__pageshowDiagnosticRecord = event => deliver({ documentId, documentTime: performance.now(), ...event })
    const snapshot = () => ({
      redirecting: window.__pageshowDiagnosticState?.redirecting,
      ready: window.__pageshowDiagnosticState?.ready,
      aborted: window.__pageshowDiagnosticLifetime?.(),
    })
    window.addEventListener('pageshow', (event) => {
      void window.__pageshowDiagnosticRecord({
        kind: 'pageshow-before',
        persisted: event.persisted,
        trusted: event.isTrusted,
        ...snapshot(),
      })
      queueMicrotask(() => {
        void window.__pageshowDiagnosticRecord({
          kind: 'pageshow-after',
          persisted: event.persisted,
          trusted: event.isTrusted,
          ...snapshot(),
        })
      })
    })
    window.addEventListener('pagehide', event => void window.__pageshowDiagnosticRecord({
      kind: 'pagehide',
      persisted: event.persisted,
      trusted: event.isTrusted,
      ...snapshot(),
    }))
    const fetch = window.fetch
    window.fetch = (...args) => {
      const pathname = typeof args[0] === 'string' ? args[0] : args[0].url
      if (pathname?.endsWith('/direct/authorize'))
        void window.__pageshowDiagnosticRecord({ kind: 'authorize-attempt', signalAborted: args[1]?.signal?.aborted, ...snapshot() })

      return fetch(...args)
    }
  })

  let failure
  try {
    await server.listen()
    origin = `http://localhost:${server.httpServer.address().port}`
    await page.goto(`${origin}${originalPath}`, { waitUntil: 'commit', timeout: 45000 })
    await Promise.race([
      redirected.promise,
      delay(45000).then(() => {
        throw new Error('Canonical production redirect was not reached')
      }),
    ])
    assert.ok(barrierResponse, 'Initial load barrier must have been requested')
    record({ kind: 'release-load-barrier', alreadyDestroyed: barrierResponse.destroyed })
    if (!barrierResponse.destroyed)
      barrierResponse.writeHead(200, { 'content-type': 'image/svg+xml' }).end('<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"/>')
    await delay(2000)

    const departure = events.find(event => event.kind === 'redirect')
    assert.ok(departure?.aborted && departure.redirecting, 'Real redirect must retire the document before navigation')
    const initialShow = events.find(event => event.kind === 'pageshow-after' && event.elapsed > departure.elapsed && event.persisted === false && event.trusted === true)
    assert.ok(initialShow, 'Native initial pageshow after redirect was not observed; diagnosis remains inconclusive')
    const lateAuthorizations = events.filter(event => event.kind === 'authorize-attempt' && event.elapsed > departure.elapsed)
    if (label === 'reference') {
      assert.equal(initialShow.aborted, false)
      assert.equal(initialShow.redirecting, false)
      assert.ok(lateAuthorizations.length > 0, 'Reference must reproduce the departing document authorization attempt')
      assert.ok(pageerrors.some(error => error.message.includes('/direct/authorize due to access control checks.')), 'Reference must reproduce the exact WebKit pageerror mechanism')
    }
    else {
      assert.equal(initialShow.aborted, true)
      assert.equal(initialShow.redirecting, true)
      assert.deepEqual(lateAuthorizations, [])
      assert.deepEqual(pageerrors, [])
    }
  }
  catch (error) {
    failure = error.stack
  }
  finally {
    if (canonicalResponse && !canonicalResponse.destroyed)
      canonicalResponse.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><title>Diagnostic navigation completed</title>')
    await context.tracing.stop({ path: path.join(output, `${label}-trace.zip`) })
    await context.close()
    await server.close()
  }

  if (!failure && label === 'candidate' && pageerrors.length)
    failure = 'Candidate emitted a pageerror during diagnostic cleanup; all errors remain preserved'
  if (!failure && label === 'reference' && pageerrors.some(error => !error.message.includes('/direct/authorize due to access control checks.')))
    failure = 'Reference emitted an additional unexpected pageerror; all errors remain preserved'

  const result = {
    label,
    sha,
    tree: git(['rev-parse', 'HEAD^{tree}'], directory),
    sourceHashes,
    events,
    pageerrors,
    failure,
    diagnosticOnly: true,
  }
  await writeFile(path.join(output, `${label}.json`), `${JSON.stringify(result, null, 2)}\n`)
  assert.equal(git(['diff', '--name-only', 'HEAD'], directory), '')
  console.log(JSON.stringify({
    label,
    sha,
    eventCount: events.length,
    pageerrorCount: pageerrors.length,
    verdict: failure ? 'not-proven' : 'controlled-observation-confirmed',
  }))
  return result
}

try {
  for (const [label, sha] of sources)
    results.push(await observe(label, sha))
}
finally {
  await browser.close()
}

assert.equal(results.length, 2)
assert.ok(results.every(result => !result.failure), 'Both the strict reference negative and candidate observation must be confirmed; inspect preserved failures')
