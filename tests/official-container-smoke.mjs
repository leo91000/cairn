// The candidate image supplies both the official binary and the SPA. Only email
// delivery is replaced; migrations, Postgres, sessions and HTTP routing are real.
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const image = process.argv[2]
const commit = process.env.SMOKE_COMMIT || process.env.GITHUB_SHA
assert.ok(image, 'Pass the exact official image under test')
assert.match(commit || '', /^[a-f0-9]{40}$/, 'Set SMOKE_COMMIT to the image build commit')
const name = `leo-official-image-${randomUUID().slice(0, 8)}`
const installationImage = process.env.SMOKE_INSTALLATION_IMAGE || `ghcr.io/leo91000/leo-agent-manager@sha256:${'a'.repeat(64)}`

async function docker(...args) {
  try {
    return await exec('docker', ['--context', 'default', ...args], { timeout: 180000 })
  }
  catch {
    // Docker errors can repeat environment arguments. Never echo raw output.
    throw new Error(`Official smoke Docker ${args[0]} failed`)
  }
}

const messages = []
const mail = createServer(async (request, response) => {
  let body = ''
  for await (const chunk of request)
    body += chunk
  messages.push(JSON.parse(body).text)
  response.writeHead(200).end('{}')
})
const reservation = createServer()
let origin

async function ready() {
  for (let attempt = 0; attempt < 150; attempt++) {
    const response = await fetch(`${origin}/health`).catch(() => null)
    if (response?.ok)
      return response
    await response?.body?.cancel()
    await setTimeout(200)
  }

  throw new Error('Exact official image did not become ready')
}

async function main() {
  try {
    mail.listen(0, '127.0.0.1')
    reservation.listen(0, '127.0.0.1')
    await Promise.all([once(mail, 'listening'), once(reservation, 'listening')])
    const port = reservation.address().port
    await new Promise(resolve => reservation.close(resolve))
    origin = `http://localhost:${port}`
    await docker('run', '-d', '--name', `${name}-db`, '-p', '127.0.0.1::5432', '-e', 'POSTGRES_USER=leo', '-e', 'POSTGRES_PASSWORD=fixture-only', '-e', 'POSTGRES_DB=leo_official', '-v', `${name}-data:/var/lib/postgresql/data`, '--health-cmd', 'pg_isready -h 127.0.0.1 -U leo -d leo_official', '--health-interval', '1s', '--health-retries', '30', 'postgres:17-alpine')
    for (let attempt = 0; ; attempt++) {
      assert.ok(attempt < 60, 'Disposable Postgres did not become ready')
      const result = await docker('inspect', '--format', '{{.State.Health.Status}}', `${name}-db`)
      if (result.stdout.trim() === 'healthy')
        break
      await setTimeout(500)
    }

    const binding = JSON.parse((await docker('inspect', '--format', '{{json .NetworkSettings.Ports}}', `${name}-db`)).stdout)
    const databasePort = binding['5432/tcp'][0].HostPort
    await docker('run', '-d', '--name', name, '--network', 'host', '-e', `LEO_OFFICIAL_DATABASE_URL=postgres://leo:fixture-only@127.0.0.1:${databasePort}/leo_official`, '-e', `LEO_OFFICIAL_ORIGIN=${origin}`, '-e', `LEO_OFFICIAL_LISTEN=127.0.0.1:${port}`, '-e', `LEO_OFFICIAL_EMAIL_ENDPOINT=http://127.0.0.1:${mail.address().port}/emails`, '-e', 'LEO_OFFICIAL_EMAIL_KEY=fixture-only', '-e', 'LEO_OFFICIAL_EMAIL_FROM=leo@example.test', '-e', `LEO_INSTALLATION_IMAGE=${installationImage}`, image)
    const healthResponse = await ready()
    assert.match(healthResponse.headers.get('cache-control'), /no-store/)
    assert.deepEqual(await healthResponse.json(), { status: 'ok', commit, runtimeId: commit })
    const user = (await docker('exec', name, 'id', '-u')).stdout.trim()
    assert.equal(user, '1000', 'Official process must run unprivileged')
    const root = await fetch(origin)
    assert.equal(root.status, 200)
    assert.equal(root.headers.get('x-frame-options'), 'DENY')
    const html = await root.text()
    assert.match(html, /<div id="app">/)
    const script = html.match(/src="([^"]+\.js)"/)[1]
    const asset = await fetch(new URL(script, origin))
    assert.equal(asset.status, 200)
    assert.match(asset.headers.get('content-type'), /javascript/)
    assert.ok((await asset.text()).length > 100)
    const deepLink = await fetch(`${origin}/claim`)
    assert.equal(await deepLink.text(), html, 'Deep links must serve the bundled official SPA')
    assert.equal((await fetch(`${origin}/api/unknown`)).status, 404)
    const release = await fetch(`${origin}/install/release`)
    assert.deepEqual(await release.json(), { image: installationImage })
    const installer = await fetch(`${origin}/install.sh`).then(response => response.text())
    assert.ok(installer.includes(origin), 'Installer must use the configured official origin')
    assert.ok(!installer.includes('__LEO_'), 'Embedded installer assets must be complete')

    const post = (path, body) => fetch(`${origin}${path}`, {
      method: 'POST',
      headers: { 'origin': origin, 'content-type': 'application/json' },
      body: JSON.stringify(body),
    })
    const requested = await post('/api/account/email-code', { email: `${name}@example.test` })
    assert.equal(requested.status, 202)
    const challenge = await requested.json()
    const verified = await post('/api/account/verify', { challenge: challenge.challenge, code: messages[0].match(/\b\d{8}\b/)[0] })
    assert.equal(verified.status, 200, 'Image must support sign-in against migrated Postgres')
    const cookie = verified.headers.get('set-cookie').split(';')[0]
    await docker('restart', name)
    await ready()
    const session = await fetch(`${origin}/api/account/session`, { headers: { cookie } })
    assert.equal(session.status, 200, 'Sessions must survive official process replacement')
    await docker('stop', `${name}-db`)
    assert.equal((await fetch(`${origin}/health`)).status, 503, 'Readiness must detect database loss')
    console.warn(`Official exact-image smoke passed: ${image}; commit ${commit}; SPA, migrations, sign-in, restart and database readiness`)
  }
  finally {
    await Promise.all([docker('rm', '-f', name).catch(() => {}), docker('rm', '-f', `${name}-db`).catch(() => {})])
    await docker('volume', 'rm', `${name}-data`).catch(() => {})
    mail.closeAllConnections()
    await new Promise(resolve => mail.close(resolve))
    if (reservation.listening)
      await new Promise(resolve => reservation.close(resolve))
  }
}

main().catch((error) => {
  console.error(error.message)
  process.exitCode = 1
})
