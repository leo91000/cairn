import { Buffer } from 'node:buffer'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
} from 'node:fs'
import { cpus, tmpdir } from 'node:os'
import { join } from 'node:path'
import { performance } from 'node:perf_hooks'
import process from 'node:process'
import { DatabaseSync } from 'node:sqlite'
import { fileURLToPath } from 'node:url'
import { prepareRunProgress, runProgressSql } from './run-progress.mjs'

const repo = fileURLToPath(new URL('..', import.meta.url))
const dir = mkdtempSync(join(tmpdir(), 'progress-cost-'))
try {
  const path = join(dir, 'manager.db')
  const db = new DatabaseSync(path)
  db.exec('PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL')
  const runs = readFileSync(join(repo, 'crates/installation/src/runs/mod.rs'), 'utf8')
  db.exec(runs.match(/r#"(CREATE TABLE runs\([\s\S]*?)"#,\n {4}projection_schema/)[1])
  const store = readFileSync(join(repo, 'crates/installation/src/store.rs'), 'utf8')
  for (const table of ['run_checkpoints', 'events'])
    db.exec(store.match(new RegExp(`CREATE TABLE (?:IF NOT EXISTS )?${table}\\([^;]+;`))[0])
  db.exec('CREATE INDEX events_run ON events(run_id,id)')
  const writer = readFileSync(join(repo, 'crates/installation/src/worker/progress.rs'), 'utf8')
  db.exec(writer.match(/SCHEMA: &str = "([^"]+)"/)[1])
  db.prepare('INSERT INTO runs(id,task_id,status,created_at,recovery_pending,resume_available,resume_count,boundary) VALUES(\'run\',\'task\',\'running\',1,0,1,0,?)').run(JSON.stringify({ chatExecution: { messageId: 'turn' } }))
  const checkpoint = JSON.stringify({ runnerId: 'attempt', process: null, prepared: { fixture: 'x'.repeat(256 * 1024) } })
  const item = { fingerprint: 'a'.repeat(64), completed: true, tool: true }
  const progress = JSON.stringify({
    version: 1,
    executionId: 'exec',
    runnerId: 'attempt',
    process: null,
    turnId: 'turn',
    startedAt: 1,
    advancedAt: 2,
    finalizedAt: null,
    phase: 'provider',
    phaseAt: 1,
    complete: true,
    items: Object.fromEntries(Array.from({ length: 128 }, (_, n) => [n.toString(16).padStart(64, '0'), item])),
    providerStarted: true,
    persistedAt: 2,
  })
  db.prepare('INSERT INTO run_checkpoints VALUES(?,?)').run('run', checkpoint)
  db.prepare('INSERT INTO run_progress VALUES(?,?)').run('run', progress)
  const event = db.prepare('INSERT INTO events(run_id,created_at,type,text) VALUES(?,?,?,?)')
  db.exec('BEGIN')
  for (let n = 0; n < 100000; n++)
    event.run('run', n + 1, 'diagnostic', 'fixture')
  db.exec('COMMIT; PRAGMA wal_checkpoint(TRUNCATE)')
  const queries = {
    baseline: db.prepare('SELECT r.id,json_extract(k.state,\'$.runnerId\') AS attemptId,json_extract(r.boundary,\'$.chatExecution.messageId\') AS activeMessageId,(SELECT e.id FROM events e WHERE e.run_id=r.id ORDER BY e.id DESC LIMIT 1) AS latestId,(SELECT e.created_at FROM events e WHERE e.run_id=r.id ORDER BY e.id DESC LIMIT 1) AS latestAt FROM runs r LEFT JOIN run_checkpoints k ON k.run_id=r.id WHERE r.id=?'),
    candidate: prepareRunProgress(db),
  }
  const measured = {}
  for (const [name, query] of Object.entries(queries)) {
    const cpu = process.cpuUsage()
    const start = performance.now()
    for (let n = 0; n < 1000; n++)
      query.get('run')
    const use = process.cpuUsage(cpu)
    measured[name] = { warmMeanMs: (performance.now() - start) / 1000, cpuMs: (use.user + use.system) / 1000, outputBytes: Buffer.byteLength(JSON.stringify(query.get('run'))) }
  }

  const reopened = new DatabaseSync(path, { readOnly: true })
  const start = performance.now()
  prepareRunProgress(reopened).get('run')
  const reopenedReadMs = performance.now() - start
  reopened.close()
  const writes = {}
  for (const [name, value, table] of [['checkpointAlternative', checkpoint, 'run_checkpoints'], ['smallProgressRow', progress, 'run_progress']]) {
    const update = db.prepare(`UPDATE ${table} SET state=? WHERE run_id='run'`)
    const encoded = Array.from({ length: 60 }, (_, n) => JSON.stringify({ ...JSON.parse(value), measurementRevision: n }))
    const cpu = process.cpuUsage()
    const start = performance.now()
    for (const row of encoded) {
      db.exec('BEGIN')
      update.run(row)
      db.exec('COMMIT')
    }

    const use = process.cpuUsage(cpu)
    writes[name] = {
      meanWriteMs: (performance.now() - start) / 60,
      cpuMs: (use.user + use.system) / 1000,
      encodedBytesPerWrite: Buffer.byteLength(value),
      encodedBytesPer60Updates: 60 * Buffer.byteLength(value),
      walBytes: statSync(`${path}-wal`).size,
    }
    db.exec('PRAGMA wal_checkpoint(TRUNCATE)')
  }

  const result = {
    fixtureOnly: true,
    environment: {
      node: process.version,
      sqlite: db.prepare('SELECT sqlite_version() AS v').get().v,
      platform: process.platform,
      cpu: cpus()[0].model,
      database: 'temporary local filesystem WAL synchronous=FULL; OS cache warm; no network/production requests',
    },
    workload: {
      diagnostics: 100000,
      items: 128,
      preparedBytes: 256 * 1024,
      reads: 1000,
      writes: 60,
      maxProgressPublicLookupsPerTick: 200,
      maxRowBytes: 24576,
      maxGlobalProbeOutputBytes: 512000,
    },
    reads: measured,
    coldConnectionOsWarmReadMs: reopenedReadMs,
    writes,
    candidatePlan: db.prepare(`EXPLAIN QUERY PLAN ${runProgressSql}`).all('run').map(row => row.detail),
    limits: 'SQL microbenchmark, not Rust classifier or fsync throughput qualification; original worker writes checkpoints on existing heartbeat, NOT per output event. Alternative compares equal update counts only; candidate adds <=1 progress write/second plus immediate phase/coverage transitions and existing heartbeat writes. Storage sizes are encoded values; no production latency claim.',
  }
  console.log(JSON.stringify(result))
  db.close()
}
finally { rmSync(dir, { recursive: true, force: true }) }
