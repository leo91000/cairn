// Real official-service adapter for the exact-image deployment smoke test.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import { createServer } from 'node:http'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

export async function startOfficial() {
  assert.ok(process.env.CAIRN_BEACON_TEST_DATABASE_URL, 'Use a disposable official Postgres database for the container smoke test')
  const messages = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body).text)
    response.writeHead(200).end('{}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const reservation = createServer()
  reservation.listen(0, '127.0.0.1')
  await once(reservation, 'listening')
  const port = reservation.address().port
  await new Promise(resolve => reservation.close(resolve))
  const origin = `http://localhost:${port}`
  const child = spawn(process.env.CAIRN_SMOKE_OFFICIAL_BINARY || 'target/debug/cairn-beacon', [], {
    env: {
      ...process.env,
      CAIRN_BEACON_DATABASE_URL: process.env.CAIRN_BEACON_TEST_DATABASE_URL,
      CAIRN_BEACON_ORIGIN: origin,
      CAIRN_BEACON_LISTEN: `127.0.0.1:${port}`,
      CAIRN_BEACON_EMAIL_ENDPOINT: `http://127.0.0.1:${mail.address().port}/emails`,
      CAIRN_BEACON_EMAIL_KEY: 'fixture-only',
      CAIRN_BEACON_EMAIL_FROM: 'cairn@example.test',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let spawnError
  let startupOutput = ''
  child.on('error', error => spawnError = error)
  for (const stream of [child.stdout, child.stderr])
    stream.on('data', chunk => startupOutput = (startupOutput + chunk).slice(-8192))

  async function stop() {
    if (!spawnError && child.exitCode === null && child.signalCode === null) {
      const exited = once(child, 'exit')
      child.kill('SIGTERM')
      await exited
    }

    await new Promise(resolve => mail.close(resolve))
  }

  try {
    for (let attempt = 0; ; attempt++) {
      if (spawnError)
        throw spawnError
      // Never quote arbitrary child output: a startup error may carry secrets.
      const startupDiagnostic = [
        'Could not connect to the official Postgres database',
        'Build the web application with pnpm build before starting the official service',
        'Could not bind CAIRN_BEACON_LISTEN',
        'Official database migration failed',
      ].find(message => startupOutput.includes(message)) || 'No safe startup diagnostic'
      assert.ok(child.exitCode === null && attempt < 100, `Official smoke service did not become ready (exit ${child.exitCode}; ${startupDiagnostic})`)
      if (await fetch(`${origin}/health`).then(response => response.ok).catch(() => false))
        break
      await setTimeout(200)
    }

    const post = (path, body, headers = {}) => fetch(`${origin}${path}`, {
      method: 'POST',
      headers: { 'origin': origin, 'content-type': 'application/json', ...headers },
      body: JSON.stringify(body),
    })
    const requested = await post('/api/account/email-code', { email: `smoke-${randomUUID()}@example.test` })
    assert.equal(requested.status, 202)
    const challenge = await requested.json()
    const verified = await post('/api/account/verify', {
      challenge: challenge.challenge,
      code: messages[0].match(/\b\d{8}\b/)[0],
    })
    assert.equal(verified.status, 200)
    const session = await verified.json()
    const headers = {
      'cookie': verified.headers.get('set-cookie').split(';')[0],
      'x-csrf-token': session.csrf,
      'content-type': 'application/json',
      'origin': origin,
    }
    const claim = await post('/api/installations/claim-code', {}, headers)
    assert.equal(claim.status, 201)
    return {
      origin,
      headers,
      claimCode: (await claim.json()).code,
      stop,
    }
  }
  catch (error) {
    await stop()
    throw error
  }
}
