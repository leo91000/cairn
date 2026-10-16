// Execute the operator backup/restore commands against an isolated database.
// Container names deliberately differ from service names; no Compose env is set.
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { readFileSync } from 'node:fs'
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
// The helper and its fixture containers must use the same daemon, including
// machines whose interactive Docker context points at Docker Desktop.
process.env.DOCKER_CONTEXT = 'default'
const compose = readFileSync(new URL('../deploy/beacon/compose.production.yaml', import.meta.url), 'utf8')
const image = compose.match(/image: (postgres:17-alpine(?:@sha256:[a-f0-9]{64})?)/)[1]
const name = `renamed-beacon-backup-${randomUUID().slice(0, 8)}`
const script = new URL('../deploy/beacon/postgres-backup.sh', import.meta.url).pathname
const automatic = new URL('../deploy/beacon/release-backup.py', import.meta.url).pathname

async function automaticRecovery(directory) {
  const beacon = `${name}-beacon`
  const recipient = join(directory, 'recovery-key')
  const backups = join(directory, 'automatic')
  const server = createServer((request, response) => {
    response.setHeader('content-type', 'application/json')
    response.end(JSON.stringify(request.url === '/health'
      ? { status: 'ok', commit: 'a'.repeat(40) }
      : { image: `ghcr.io/fixture/cairn@sha256:${'b'.repeat(64)}` }))
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  try {
    await exec('ssh-keygen', ['-q', '-t', 'ed25519', '-N', '', '-f', recipient])
    await docker('run', '-d', '--name', beacon, '--label', `com.docker.compose.project=${name}`, '--label', 'com.docker.compose.service=beacon', '--entrypoint', 'sh', image, '-c', 'trap "exit 0" TERM; while :; do sleep 1; done')
    const config = {
      project: name,
      origin: `http://127.0.0.1:${server.address().port}`,
      directory: backups,
      helper: script,
      recipient: `${recipient}.pub`,
    }
    await mkdir(backups, { mode: 0o700 })
    const identifier = randomUUID().replaceAll('-', '')
    const code = 'import importlib.util,json,os,sys; s=importlib.util.spec_from_file_location("backup",sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); os.umask(0o077); m.backup(json.loads(sys.argv[2]),"v0.56.2",sys.argv[3])'
    await exec('python3', ['-c', code, automatic, JSON.stringify(config), identifier], { timeout: 180000 })
    const proof = JSON.parse(await readFile(join(backups, identifier, 'release.json'), 'utf8'))
    const ciphertext = join(backups, identifier, 'beacon.dump.age')
    assert.equal(proof.restore.verified, true)
    assert.equal(proof.restore.sourceTables, proof.restore.restoredTables)
    assert.equal(proof.encryptedSha256, createHash('sha256').update(await readFile(ciphertext)).digest('hex'))
    const plain = join(directory, 'automatic-decrypted.dump')
    await exec('age', ['--decrypt', '-i', recipient, '-o', plain, ciphertext])
    assert.ok((await stat(plain)).size > 0, 'The operator key must decrypt the automated recovery artifact')
    await assert.rejects(stat(join(backups, identifier, 'beacon.dump')), 'Completed automated backups retain only ciphertext')
    assert.equal((await docker('inspect', '--format', '{{.State.Running}}', beacon)).stdout.trim(), 'true')
    console.warn('Automatic recovery passed: real stop/resume, isolated restore, encryption, matching operator-key decryption and private retention')
  }
  finally {
    await new Promise(resolve => server.close(resolve))
    await docker('rm', '-fv', beacon).catch(() => {})
  }
}

async function docker(...args) {
  try {
    return await exec('docker', ['--context', 'default', ...args], { timeout: 180000 })
  }
  catch {
    throw new Error(`Disposable backup test Docker ${args[0]} failed`)
  }
}

async function main() {
  const directory = await mkdtemp(join(tmpdir(), 'cairn-beacon-backup-test-'))
  const dump = join(directory, 'beacon.dump')

  try {
    await docker('run', '-d', '--name', name, '--label', `com.docker.compose.project=${name}`, '--label', 'com.docker.compose.service=postgres', '-e', 'POSTGRES_USER=cairn', '-e', 'POSTGRES_PASSWORD=fixture-only', '-e', 'POSTGRES_DB=cairn_beacon', image)
    for (let attempt = 0; ; attempt++) {
      assert.ok(attempt < 60, 'Disposable backup database did not start')
      try {
        await docker('exec', name, 'pg_isready', '-h', '127.0.0.1', '-U', 'cairn', '-d', 'cairn_beacon')
        break
      }
      catch {
        await setTimeout(500)
      }
    }

    const sql = command => docker('exec', name, 'psql', '-U', 'cairn', '-d', 'cairn_beacon', '-v', 'ON_ERROR_STOP=1', '-Atc', command)
    await sql('CREATE TABLE backup_fixture (id integer PRIMARY KEY, value text NOT NULL); INSERT INTO backup_fixture VALUES (113, \'known restore value\');')
    await exec('bash', [script, 'backup', name, dump])
    assert.equal((await stat(dump)).mode & 0o777, 0o600, 'Dump must remain private')
    assert.ok((await stat(dump)).size > 0)
    const originalDump = await readFile(dump)
    await assert.rejects(exec('bash', [script, 'backup', name, dump]), 'A later backup must not overwrite the previous recovery point')
    assert.deepEqual(await readFile(dump), originalDump)
    await sql('DROP TABLE backup_fixture;')
    await exec('bash', [script, 'restore', name, dump])
    assert.equal((await sql('SELECT id, value FROM backup_fixture;')).stdout.trim(), '113|known restore value')
    await assert.rejects(exec('bash', [script, 'restore', name, dump]), 'Restore must fail on a nonempty target instead of ignoring errors')
    await writeFile(join(directory, 'invalid.dump'), 'invalid fixture')
    await assert.rejects(exec('bash', [script, 'restore', name, join(directory, 'invalid.dump')]))
    await assert.rejects(exec('bash', [script, 'backup', `${name}-missing`, join(directory, 'failed.dump')]))
    await assert.rejects(stat(join(directory, 'failed.dump')), 'A failed backup must not leave a usable-looking dump')
    console.warn('Beacon Postgres backup/restore passed: renamed container, private dump, round trip and failure handling')
    await automaticRecovery(directory)
  }
  finally {
    await docker('rm', '-fv', name).catch(() => {})
    await rm(directory, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error.message)
  process.exitCode = 1
})
