import { readFileSync } from 'node:fs'
import { DatabaseSync } from 'node:sqlite'
import { describe, expect, it } from 'vitest'
import {
  canonicalWaitSql,
  decodeRunProgress,
  observeRunProgress,
  prepareRunProgress,
  progressOccurrence,
} from '../run-progress.mjs'

const now = 4_000_000
const old = now - 2_000_000

function fixture({ legacy = false } = {}) {
  const db = new DatabaseSync(':memory:')
  // Extract the actual writer schemas, rather than approximating their constraints.
  const runs = readFileSync(new URL('../../crates/installation/src/runs/mod.rs', import.meta.url), 'utf8')
  db.exec(runs.match(/r#"(CREATE TABLE runs\([\s\S]*?)"#,\n {4}projection_schema/)[1])
  const attempts = readFileSync(new URL('../../crates/installation/src/nodes/attempts.rs', import.meta.url), 'utf8')
  db.exec(attempts.match(/SCHEMA: &str = "([\s\S]*?)";/)[1])
  const store = readFileSync(new URL('../../crates/installation/src/store.rs', import.meta.url), 'utf8')
  db.exec(store.match(/CREATE TABLE run_checkpoints\([^;]+;/)[0])
  if (!legacy) {
    const source = readFileSync(new URL('../../crates/installation/src/worker/progress.rs', import.meta.url), 'utf8')
    db.exec(source.match(/SCHEMA: &str = "([^"]+)"/)[1])
  }

  db.prepare(`INSERT INTO runs(id,task_id,status,created_at,started_at,recovery_pending,resume_available,resume_count,boundary)
    VALUES('run','task','running',?, ?,0,1,0,?)`).run(old, old, JSON.stringify({ chatExecution: { messageId: 'turn' } }))
  let checkpoint = { runnerId: 'attempt', process: { pid: 7, start: 'start', boot: 'boot' }, completed: false }
  let progress = {
    version: 1,
    executionId: 'execution',
    runnerId: 'attempt',
    process: checkpoint.process,
    turnId: 'turn',
    startedAt: old,
    advancedAt: old,
    phase: 'provider',
    phaseAt: old,
    complete: true,
    items: { 'private-digest': 'hidden' },
  }
  const write = (c = {}, p = {}) => {
    checkpoint = { ...checkpoint, ...c }
    progress = { ...progress, ...p }
    db.prepare('INSERT OR REPLACE INTO run_checkpoints VALUES(?,?)').run('run', JSON.stringify(checkpoint))
    if (!legacy)
      db.prepare('INSERT OR REPLACE INTO run_progress VALUES(?,?)').run('run', JSON.stringify(progress))
  }

  write()
  const read = () => decodeRunProgress(prepareRunProgress(db).get('run'))
  return { db, write, read }
}

describe('writer progress → safe SQLite projection → policy', () => {
  it('uses exact indexed key lookups and exports no payload or digests', () => {
    const { db, read } = fixture()
    const run = read()
    expect(observeRunProgress(run, now)).toMatchObject({ status: 'bad', phase: 'provider', at: old })
    expect(JSON.stringify(run)).not.toContain('private-digest')
    expect(JSON.stringify(run)).not.toContain('hidden')
    const plan = db.prepare(`EXPLAIN QUERY PLAN ${prepareRunProgress(db).sourceSQL}`).all('run').map(row => row.detail).join('\n')
    expect(plan).not.toMatch(/SCAN/)
    db.close()
  })

  it('keeps legacy, mismatched attempts, missing turns and pending resume unknown', () => {
    const legacy = fixture({ legacy: true })
    expect(observeRunProgress(legacy.read(), now).status).toBe('unknown')
    legacy.db.close()
    const { db, read, write } = fixture()
    for (const [checkpoint, progress] of [[{ runnerId: 'new' }, {}], [{ runnerId: 'attempt', turnPending: true }, {}], [{ turnPending: false }, { turnId: 'previous' }], [{}, { turnId: null }], [{}, { version: 2 }]]) {
      write(checkpoint, progress)
      expect(observeRunProgress(read(), now).status).toBe('unknown')
    }

    db.close()
  })

  it('allows real work and new attempts their own clock, then keeps finalization sticky', () => {
    const { db, read, write } = fixture()
    write({}, { advancedAt: now - 1000 })
    expect(observeRunProgress(read(), now).status).toBe('good')
    write({ runnerId: 'replacement' }, {
      runnerId: 'replacement',
      executionId: 'new',
      startedAt: now - 1000,
      advancedAt: null,
      phase: 'preparing',
      phaseAt: now - 1000,
    })
    expect(observeRunProgress(read(), now).status).toBe('unknown')
    write({}, { phase: 'provider', advancedAt: now, phaseAt: now })
    expect(observeRunProgress(read(), now).status).toBe('good')
    write({ completed: false }, {
      startedAt: old,
      phase: 'finalizing',
      phaseAt: old,
      advancedAt: now,
    })
    expect(observeRunProgress(read(), now)).toMatchObject({ status: 'bad', phase: 'finalizing' })
    db.close()
  })

  it('treats quiet tools and exhausted coverage as advisory, never recovery', () => {
    const { db, read, write } = fixture()
    write({}, { phase: 'tool' })
    expect(observeRunProgress(read(), now)).toMatchObject({ status: 'expected', phase: 'quiet_tool' })
    write({}, { complete: false, advancedAt: now })
    expect(observeRunProgress(read(), now).status).toBe('unknown')
    write({}, { complete: true, advancedAt: now + 1 })
    expect(observeRunProgress(read(), now).status).toBe('unknown')
    db.close()
  })

  it('separates mission turns and full local process instances', () => {
    const { db, read, write } = fixture()
    db.prepare('UPDATE runs SET boundary=?').run(JSON.stringify({ missionTurnId: 'mission' }))
    write({ runnerId: null }, { runnerId: null, turnId: 'mission', advancedAt: now })
    expect(observeRunProgress(read(), now).status).toBe('good')
    const episode = progressOccurrence(read())
    write({ process: { pid: 7, start: 'other', boot: 'boot' } })
    expect(observeRunProgress(read(), now).status).toBe('unknown')
    expect(progressOccurrence(read())).not.toBe(episode)
    db.close()
  })

  it('requires actual terminal run settlement, not completion flags or intent', () => {
    const { db, read, write } = fixture()
    write({ completed: true, terminalCode: 0, settled: { status: 'succeeded' } }, { advancedAt: now })
    expect(observeRunProgress(read(), now).status).toBe('bad')
    db.prepare('UPDATE runs SET status=\'succeeded\',finished_at=?').run(now)
    write({ process: null })
    expect(observeRunProgress(read(), now).status).toBe('unknown')
    write({}, { finalizedAt: now })
    expect(observeRunProgress(read(), now)).toMatchObject({ status: 'good', phase: 'terminal' })
    db.close()
  })
})

it('accepts only future canonical waits for the current resumable owner and turn', () => {
  const { db, read } = fixture()
  const chats = readFileSync(new URL('../../crates/installation/src/chats/storage.rs', import.meta.url), 'utf8')
  db.exec('CREATE TABLE agents(id TEXT PRIMARY KEY); CREATE TABLE projects(id TEXT PRIMARY KEY);')
  db.exec(chats.match(/CREATE TABLE conversations\([\s\S]*?\n\);/)[0])
  const waits = readFileSync(new URL('../../crates/installation/src/waits/sql.rs', import.meta.url), 'utf8')
  db.exec(waits.match(/CREATE TABLE durable_wakeups\([\s\S]*?\n\);/)[0])
  db.exec('INSERT INTO conversations(id,slug,title,agent_id,run_id,paused,lifecycle,created_at,updated_at,revision,session_restart_requested,cancelled_by_deletion,persisted_at) VALUES(\'chat\',\'slug\',\'\',\'agent\',\'run\',0,\'active\',0,0,\'revision\',0,0,0)')
  db.prepare('INSERT INTO durable_wakeups(chat_id,id,message_id,reason,seconds,created_at,wake_at) VALUES(\'chat\',\'wait\',\'turn\',\'\',60,?,?)').run(now, now + 60000)
  db.exec(chats.match(/CREATE UNIQUE INDEX conversations_run[^;]+;/)[0])
  for (const name of ['chat', 'mission'])
    db.exec(waits.match(new RegExp(`CREATE UNIQUE INDEX durable_wakeups_${name}[^;]+;`))[0])
  const plan = db.prepare(`EXPLAIN QUERY PLAN ${canonicalWaitSql}`).all('run', now, now).map(row => row.detail).join('\n')
  expect(plan).not.toMatch(/SCAN [rwc]\b/)
  const valid = () => db.prepare(canonicalWaitSql).get('run', now, now).valid
  expect(valid()).toBe(0)
  db.exec('UPDATE runs SET status=\'succeeded\'')
  expect(valid()).toBe(1)
  expect(observeRunProgress({ ...read(), progress: null, validFutureWait: valid() }, now)).toMatchObject({ status: 'expected', phase: 'waiting' })
  for (const statement of ['UPDATE conversations SET paused=1', 'UPDATE conversations SET paused=0,session_restart_requested=1', 'UPDATE conversations SET session_restart_requested=0,lifecycle=\'trash\'']) {
    db.exec(statement)
    expect(valid()).toBe(0)
  }

  db.exec('UPDATE conversations SET lifecycle=\'active\'; UPDATE durable_wakeups SET message_id=\'previous\'')
  expect(valid()).toBe(0)
  db.exec('UPDATE durable_wakeups SET message_id=\'turn\',wake_at=0')
  expect(valid()).toBe(0)
  db.close()
})

it('keeps a settled mission wait advisory while overdue wakeups retain their own signal', () => {
  const { db, read, write } = fixture()
  db.prepare('UPDATE runs SET status=\'waiting\',boundary=?').run(JSON.stringify({ missionTurnId: 'mission' }))
  write({ process: null, terminalCode: 0 }, { turnId: 'mission', phase: 'finalizing', finalizedAt: now })
  expect(observeRunProgress({ ...read(), validFutureWait: 1 }, now)).toMatchObject({ status: 'expected', phase: 'waiting' })
  expect(observeRunProgress({ ...read(), validFutureWait: 0 }, now)).toMatchObject({ status: 'unknown', phase: 'waiting' })
  db.close()
})

it('requires an attempt-bound physical certificate for failed and code-zero outcomes', () => {
  const { db, read, write } = fixture()
  db.exec('UPDATE runs SET status=\'failed\'')
  write({ process: null, terminalCode: null, completed: true }, { phase: 'finalizing' })
  for (const code of [null, 0, 17, 70, 143]) {
    write({ terminalCode: code })
    expect(observeRunProgress({ ...read(), released: 1 }, now).status).toBe('unknown')
  }

  write({ terminalCode: 17 }, { finalizedAt: now })
  expect(observeRunProgress(read(), now).status).toBe('good')
  write({ finalizationFailureAttempt: 'attempt' })
  expect(observeRunProgress(read(), now).status).toBe('bad')
  write({ finalizationFailureAttempt: null, terminalCode: 143 })
  expect(observeRunProgress(read(), now).status).toBe('good')
  write({ runnerId: 'replacement' })
  expect(observeRunProgress(read(), now).status).toBe('unknown')
  db.close()
})

it('keeps new-manager/old-node terminal settlement and queued admission unknown', () => {
  const { db, read, write } = fixture()
  db.exec('UPDATE runs SET status=\'succeeded\'')
  write({ process: null, terminalCode: 0 }, { phase: 'finalizing', finalizedAt: null })
  expect(observeRunProgress(read(), now)).toMatchObject({ status: 'unknown', phase: 'finalizing' })
  write({ terminalCode: 70, finalizationFailureAttempt: 'attempt' })
  expect(observeRunProgress(read(), now).status).toBe('bad')
  db.exec('UPDATE runs SET status=\'queued\'')
  write({ terminalCode: null, finalizationFailureAttempt: null }, { phase: 'provider' })
  expect(observeRunProgress(read(), now).status).toBe('unknown')
  db.close()
})

it('preserves authoritative legacy flush failures independently of absent progress coverage', () => {
  const { db, read, write } = fixture({ legacy: true })
  write({ finalizationFailureAttempt: 'attempt', terminalCode: 70 })
  for (const status of ['running', 'waiting', 'failed']) {
    db.prepare('UPDATE runs SET status=?').run(status)
    expect(observeRunProgress(read(), now)).toMatchObject({ status: 'bad', phase: 'failed_flush' })
  }

  write({ turnPending: true })
  expect(observeRunProgress(read(), now).status).toBe('unknown')
  write({ turnPending: false, runnerId: 'replacement' })
  expect(observeRunProgress(read(), now).status).toBe('unknown')
  db.close()
})

it('keeps relay or parse gaps unknown without hiding positive finalization failure', () => {
  const { db, read, write } = fixture()
  write({ outputInterrupted: true }, { advancedAt: now })
  expect(observeRunProgress(read(), now).status).toBe('unknown')
  write({ finalizationFailureAttempt: 'attempt', terminalCode: 70 })
  expect(observeRunProgress(read(), now).status).toBe('bad')
  write({ finalizationFailureAttempt: null, terminalCode: 0, process: null }, { finalizedAt: now })
  db.exec('UPDATE runs SET status=\'succeeded\'')
  expect(observeRunProgress(read(), now).status).toBe('good')
  db.close()
})

it('does not certify unsupported checkpoint scalar types', () => {
  const { db, read, write } = fixture()
  write({}, { advancedAt: now })
  for (const mutation of [{ turnPending: 'true' }, { outputInterrupted: 'true' }, { providerCompleted: 2 }, { recoveryPending: 'false' }, { attemptId: 0 }, { terminalCode: -1 }])
    expect(observeRunProgress({ ...read(), ...mutation }, now).status).toBe('unknown')
  db.close()
})
