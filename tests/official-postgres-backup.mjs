// Execute the operator backup/restore commands against an isolated database.
// Container names deliberately differ from service names; no Compose env is set.
import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { readFileSync } from 'node:fs'
import {
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const compose = readFileSync(new URL('../deploy/official/compose.production.yaml', import.meta.url), 'utf8')
const image = compose.match(/image: (postgres:17-alpine(?:@sha256:[a-f0-9]{64})?)/)[1]
const name = `renamed-official-backup-${randomUUID().slice(0, 8)}`
const script = new URL('../deploy/official/postgres-backup.sh', import.meta.url).pathname

async function docker(...args) {
  try {
    return await exec('docker', ['--context', 'default', ...args], { timeout: 180000 })
  }
  catch {
    throw new Error(`Disposable backup test Docker ${args[0]} failed`)
  }
}

async function main() {
  const directory = await mkdtemp(join(tmpdir(), 'leo-official-backup-test-'))
  const dump = join(directory, 'official.dump')

  try {
    await docker('run', '-d', '--name', name, '-e', 'POSTGRES_USER=leo', '-e', 'POSTGRES_PASSWORD=fixture-only', '-e', 'POSTGRES_DB=leo_official', image)
    for (let attempt = 0; ; attempt++) {
      assert.ok(attempt < 60, 'Disposable backup database did not start')
      try {
        await docker('exec', name, 'pg_isready', '-h', '127.0.0.1', '-U', 'leo', '-d', 'leo_official')
        break
      }
      catch {
        await setTimeout(500)
      }
    }

    const sql = command => docker('exec', name, 'psql', '-U', 'leo', '-d', 'leo_official', '-v', 'ON_ERROR_STOP=1', '-Atc', command)
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
    console.warn('Official Postgres backup/restore passed: renamed container, private dump, round trip and failure handling')
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
