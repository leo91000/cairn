import type { Buffer } from 'node:buffer'
import type { ChildProcess } from 'node:child_process'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { setTimeout as delay } from 'node:timers/promises'
import { NativeFixture } from './native-fixture'

// Shared by journeys-beacon-relay and the network bench: real processes, fake mail only.
export async function beaconRelayFixture(port = 4395, listen = '127.0.0.1', directory = tmpdir(), originHost = 'localhost') {
  const messages: string[] = []
  const mail = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    messages.push(JSON.parse(body).text)
    response.writeHead(200, { 'content-type': 'application/json' }).end('{}')
  })
  mail.listen(0, '127.0.0.1')
  await once(mail, 'listening')
  const mailPort = (mail.address() as { port: number }).port
  const root = await mkdtemp(join(directory, 'cairn-beacon-relay-'))
  await Promise.all([mkdir(join(root, 'data')), mkdir(join(root, 'home'))])
  const url = `http://${originHost}:${port}`
  const children: ChildProcess[] = []
  const seed = new NativeFixture({
    dataDir: join(root, 'data'),
    home: join(root, 'home'),
    workspaceRoots: [root],
    workerEnabled: false,
    logger: false,
  })

  function start(binary: string, env: NodeJS.ProcessEnv) {
    const executable = binary === 'target/debug/cairn' ? process.env.CAIRN_NETWORK_INSTALLATION_BINARY || binary : binary
    const child = spawn(executable, [], { env: { ...process.env, ...env }, stdio: ['ignore', 'ignore', 'ignore'] })
    children.push(child)
    return child
  }

  async function stop(child: ChildProcess) {
    if (child.pid === undefined || child.exitCode !== null || child.signalCode !== null)
      return
    const exited = once(child, 'exit')
    child.kill('SIGTERM')
    const force = setTimeout(() => child.kill('SIGKILL'), 10000)
    try {
      await exited
    }
    finally {
      clearTimeout(force)
    }
  }

  function beacon(databaseUrl = process.env.CAIRN_BEACON_TEST_DATABASE_URL, overrides: NodeJS.ProcessEnv = {}) {
    const env = {
      CAIRN_BEACON_DATABASE_URL: databaseUrl,
      CAIRN_BEACON_ORIGIN: url,
      CAIRN_BEACON_LISTEN: `${listen}:${port}`,
      CAIRN_BEACON_EMAIL_ENDPOINT: `http://127.0.0.1:${mailPort}/emails`,
      CAIRN_BEACON_EMAIL_KEY: 'fixture-only',
      CAIRN_BEACON_EMAIL_FROM: 'cairn@example.test',
      ...overrides,
    }
    const childEnv = { ...process.env, ...env }
    const child = spawn('target/debug/cairn-beacon', [], { env: childEnv, stdio: ['ignore', 'pipe', 'pipe'] })
    children.push(child)
    let output = ''
    let listening = false
    const retain = (chunk: Buffer) => {
      output = (output + chunk.toString()).slice(-16384)
      listening ||= output.includes('Beacon listening')
    }

    child.stdout.on('data', retain)
    child.stderr.on('data', retain)
    let spawnError: Error | undefined
    child.on('error', error => spawnError = error)
    const closed = new Promise<void>(resolve => child.once('close', () => resolve()))
    const diagnostics = () => {
      let safeOutput = output.replaceAll(/(https?:\/\/|postgres(?:ql)?:\/\/)[^\s/@][^\s/:@]*:[^\s/@]+@/gi, '$1<REDACTED>@')
      for (const [key, value] of Object.entries(childEnv)) {
        if (value && value.length >= 4 && /password|secret|token|credential|api.?key/i.test(key))
          safeOutput = safeOutput.replaceAll(value, '<REDACTED>')
      }

      return { output: safeOutput, spawnError, listening }
    }

    return Object.assign(child, { diagnostics, closed })
  }

  async function waitForBeacon(child: ReturnType<typeof beacon>, timeoutMs = 60000) {
    const deadline = Date.now() + timeoutMs
    while (Date.now() < deadline) {
      if (child.exitCode !== null || child.signalCode !== null || child.diagnostics().spawnError) {
        await child.closed
        const { output, spawnError } = child.diagnostics()
        throw new Error(`Beacon exited (code ${child.exitCode}, signal ${child.signalCode}): ${spawnError?.message ?? ''}\n${output}`)
      }

      // A healthy response from a different process must not establish readiness.
      const { listening } = child.diagnostics()
      const remainingMs = deadline - Date.now()
      if (remainingMs <= 0)
        break
      const ready = listening && await fetch(`${url}/health`, { signal: AbortSignal.timeout(Math.min(1000, remainingMs)) }).then(response => response.ok).catch(() => false)
      if (ready && Date.now() < deadline && child.exitCode === null && child.signalCode === null && !child.diagnostics().spawnError)
        return

      const waitMs = Math.min(100, deadline - Date.now())
      if (waitMs > 0)
        await delay(waitMs)
    }

    throw new Error(`Beacon did not become ready within ${timeoutMs}ms.\n${child.diagnostics().output}`)
  }

  async function close() {
    await Promise.all(children.map(stop))
    await new Promise<void>(resolve => mail.close(() => resolve()))
    await rm(root, { recursive: true, force: true })
  }

  return {
    root,
    url,
    messages,
    seed,
    start,
    stop,
    beacon,
    waitForBeacon,
    close,
  }
}

export function executeBeaconSql(database: URL, sql: string) {
  execFileSync('psql', ['-v', 'ON_ERROR_STOP=1', '-c', sql], {
    env: {
      ...process.env,
      PGHOST: database.hostname,
      PGPORT: database.port || '5432',
      PGUSER: decodeURIComponent(database.username),
      PGPASSWORD: decodeURIComponent(database.password),
      PGDATABASE: decodeURIComponent(database.pathname.slice(1)),
      PGOPTIONS: database.searchParams.get('options') || '',
    },
    stdio: 'pipe',
  })
}

export function expireAccountProof(database: URL, email: string) {
  const emailLiteral = email.replaceAll('\'', '\'\'')
  const emailDigest = createHash('sha256').update(email).digest('hex')
  executeBeaconSql(database, `
    UPDATE web_sessions SET last_proof_at = NULL
    WHERE account_id IN (SELECT id FROM cairn_accounts WHERE email = '${emailLiteral}');
    UPDATE account_rate_limits SET resets_at = now() - interval '1 second'
    WHERE key = 'email:${emailDigest}'
  `)
}
