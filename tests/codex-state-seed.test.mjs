import { Buffer } from 'node:buffer'
import { mkdtemp, readFile, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { expect, it } from 'vitest'
import { emptyDatabase } from '../deploy/codex-state/prepare.mjs'

it('copies migrated schemas while removing every runtime row and retaining migration checksums', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-schema-test-'))
  const source = path.join(root, 'source.sqlite')
  const destination = path.join(root, 'template.sqlite')
  const original = new DatabaseSync(source)
  try {
    original.exec(`
      PRAGMA journal_mode=WAL;
      CREATE TABLE _sqlx_migrations(version INTEGER PRIMARY KEY, checksum BLOB);
      INSERT INTO _sqlx_migrations VALUES (37, X'010203');
      CREATE TABLE threads(id TEXT PRIMARY KEY, content TEXT);
      INSERT INTO threads VALUES ('synthetic-thread', 'fixture-only-private-content');
      CREATE TABLE logs(message TEXT);
      INSERT INTO logs VALUES ('fixture-only-private-log');
    `)
    await emptyDatabase(source, destination)
    const copy = new DatabaseSync(destination)
    try {
      expect(copy.prepare('PRAGMA integrity_check').get().integrity_check).toBe('ok')
      expect(copy.prepare('PRAGMA journal_mode').get().journal_mode).toBe('delete')
      expect(copy.prepare('SELECT version, hex(checksum) AS checksum FROM _sqlx_migrations').get()).toEqual({ version: 37, checksum: '010203' })
      for (const name of ['threads', 'logs'])
        expect(copy.prepare(`SELECT count(*) AS count FROM ${name}`).get().count).toBe(0)
      copy.exec('INSERT INTO threads VALUES (\'new-thread\', \'new content\')')
      expect(copy.prepare('SELECT content FROM threads').get().content).toBe('new content')
    }
    finally {
      copy.close()
    }

    const bytes = await readFile(destination)
    expect(bytes.includes(Buffer.from('fixture-only-private-content'))).toBe(false)
    expect(bytes.includes(Buffer.from('fixture-only-private-log'))).toBe(false)
  }
  finally {
    original.close()
    await rm(root, { recursive: true, force: true })
  }
})
