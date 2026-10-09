// Build-time schema templates only. No account, model request or user state.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import {
  mkdir,
  mkdtemp,
  readdir,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'
import { backup, DatabaseSync } from 'node:sqlite'
import { pathToFileURL } from 'node:url'

export async function emptyDatabase(source, destination) {
  const db = new DatabaseSync(source)
  try {
    db.exec('PRAGMA foreign_keys=OFF; PRAGMA secure_delete=ON')
    const tables = db.prepare(`SELECT name, type FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%'`).all()
    assert.ok(tables.every(table => table.type === 'table'), 'Schema templates require ordinary tables')
    assert.ok(tables.some(table => table.name === '_sqlx_migrations'), 'Expected native migration metadata')
    assert.ok(db.prepare('SELECT count(*) AS count FROM _sqlx_migrations').get().count > 0)
    for (const { name } of tables) {
      if (name === '_sqlx_migrations')
        continue
      assert.match(name, /^\w+$/)
      db.exec(`DELETE FROM "${name}"`)
    }

    for (const { name } of tables) {
      if (name !== '_sqlx_migrations')
        assert.equal(db.prepare(`SELECT count(*) AS count FROM "${name}"`).get().count, 0)
    }

    db.exec('VACUUM')
    assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check, 'ok')
    await backup(db, destination)
    const template = new DatabaseSync(destination)
    try {
      // A read-only image cannot create WAL sidecars. Codex restores WAL mode
      // when it opens the privately installed copy through its normal pool.
      template.exec('PRAGMA journal_mode=DELETE; VACUUM')
      assert.equal(template.prepare('PRAGMA integrity_check').get().integrity_check, 'ok')
    }
    finally {
      template.close()
    }
  }
  finally {
    db.close()
  }
}

async function initialize(home) {
  const child = spawn('codex', [
    '-c',
    'features.plugins=false',
    '-c',
    'model_provider="schema_fixture"',
    '-c',
    'model_providers.schema_fixture.name="Schema fixture"',
    '-c',
    'model_providers.schema_fixture.base_url="http://127.0.0.1:1"',
    'app-server',
  ], {
    cwd: home,
    env: { PATH: process.env.PATH, HOME: home, CODEX_HOME: home },
    stdio: ['pipe', 'pipe', 'ignore'],
  })
  const exited = once(child, 'exit')
  const lines = createInterface({ input: child.stdout })
  const send = message => child.stdin.write(`${JSON.stringify(message)}\n`)
  try {
    await new Promise((resolve, reject) => {
      const deadline = setTimeout(() => reject(new Error('Codex schema initialization timed out')), 20000)
      const finish = (error) => {
        clearTimeout(deadline)
        if (error)
          reject(error)
        else
          resolve()
      }

      child.once('error', finish)
      child.once('exit', () => finish(new Error('Codex exited before initializing its schema')))
      lines.on('line', (line) => {
        const message = JSON.parse(line)
        if (message.error) {
          finish(new Error('Codex refused schema initialization'))
          return
        }

        if (message.id === 1) {
          send({ method: 'initialized' })
          send({
            id: 2,
            method: 'thread/start',
            params: {
              cwd: home,
              model: 'gpt-6.1-sol',
              approvalPolicy: 'never',
              sandbox: 'danger-full-access',
            },
          })
        }

        if (message.id === 2)
          finish()
      })
      send({ id: 1, method: 'initialize', params: { clientInfo: { name: 'cairn_schema_builder', version: '1' }, capabilities: { experimentalApi: true } } })
    })
  }
  finally {
    child.kill('SIGTERM')
    const force = setTimeout(() => child.kill('SIGKILL'), 2000)
    await exited
    clearTimeout(force)
    lines.close()
  }
}

export async function prepare(destination) {
  const home = await mkdtemp(path.join(os.tmpdir(), 'cairn-codex-schema-'))
  try {
    await initialize(home)
    const names = (await readdir(home)).filter(name => /^[a-z_]+_\d+\.sqlite$/.test(name)).sort()
    assert.ok(names.some(name => /^state_\d+\.sqlite$/.test(name)))
    await mkdir(destination, { recursive: true })
    for (const name of names)
      await emptyDatabase(path.join(home, name), path.join(destination, name))
    await writeFile(path.join(destination, 'manifest.json'), JSON.stringify({ version: 1, files: names }))
    process.stdout.write(`${JSON.stringify({ operation: 'codex_schema_build', databases: names.length })}\n`)
  }
  finally {
    await rm(home, { recursive: true, force: true })
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  assert.ok(process.argv[2], 'Expected schema template destination')
  prepare(process.argv[2]).catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
