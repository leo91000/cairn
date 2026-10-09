import type { ChildProcess } from 'node:child_process'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { once } from 'node:events'
import { mkdir, mkdtemp, rm } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { config as loadConfig } from '../legacy/server/config'
import { Service as SeedService } from '../legacy/server/service'
import { Store } from '../legacy/server/store'

// Shared by journeys-official-relay and the network bench: real processes, fake mail only.
export async function officialRelayFixture(port = 4395, listen = '127.0.0.1', directory = tmpdir()) {
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
  const url = `http://localhost:${port}`
  const children: ChildProcess[] = []
  // As in the native browser fixtures, open the seeding module before the
  // native process applies newer database migrations.
  const seed = new SeedService(new Store(join(root, 'data')), loadConfig({
    dataDir: join(root, 'data'),
    home: join(root, 'home'),
    workspaceRoots: [root],
    workerEnabled: false,
    logger: false,
  }))

  function start(binary: string, env: NodeJS.ProcessEnv) {
    const executable = binary === 'target/debug/cairn' ? process.env.CAIRN_NETWORK_INSTALLATION_BINARY || binary : binary
    const child = spawn(executable, [], { env: { ...process.env, ...env }, stdio: ['ignore', 'ignore', 'ignore'] })
    children.push(child)
    return child
  }

  async function stop(child: ChildProcess) {
    if (child.exitCode !== null || child.signalCode !== null)
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

  function official(databaseUrl = process.env.CAIRN_BEACON_TEST_DATABASE_URL) {
    return start('target/debug/cairn-beacon', {
      CAIRN_BEACON_DATABASE_URL: databaseUrl,
      CAIRN_BEACON_ORIGIN: url,
      CAIRN_BEACON_LISTEN: `${listen}:${port}`,
      CAIRN_BEACON_EMAIL_ENDPOINT: `http://127.0.0.1:${mailPort}/emails`,
      CAIRN_BEACON_EMAIL_KEY: 'fixture-only',
      CAIRN_BEACON_EMAIL_FROM: 'cairn@example.test',
    })
  }

  async function close() {
    await Promise.all(children.map(stop))
    await seed.accounts.close()
    seed.store.close()
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
    official,
    close,
  }
}

export function executeOfficialSql(database: URL, sql: string) {
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
  executeOfficialSql(database, `
    UPDATE web_sessions SET last_proof_at = NULL
    WHERE account_id IN (SELECT id FROM cairn_accounts WHERE email = '${emailLiteral}');
    UPDATE account_rate_limits SET resets_at = now() - interval '1 second'
    WHERE key = 'email:${emailDigest}'
  `)
}
