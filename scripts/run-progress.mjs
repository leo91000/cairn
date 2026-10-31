// Safe read-only projection. No event payload, prompt, command, output or digest
// crosses this boundary. A checkpoint is an observation, never itself progress.
export const progressProjection = `json_object(
  'version',json_extract(p.state,'$.version'),
  'executionId',json_extract(p.state,'$.executionId'),
  'runnerId',json_extract(p.state,'$.runnerId'),
  'process',json_extract(p.state,'$.process'),
  'turnId',json_extract(p.state,'$.turnId'),
  'startedAt',json_extract(p.state,'$.startedAt'),
  'advancedAt',json_extract(p.state,'$.advancedAt'),
  'phase',json_extract(p.state,'$.phase'),
  'phaseAt',json_extract(p.state,'$.phaseAt'),
  'complete',json_extract(p.state,'$.complete'))`

export const runProgressSql = `SELECT r.id,r.status,r.finished_at AS finishedAt,
  json_extract(r.boundary,'$.chatExecution.messageId') AS activeMessageId,
  json_extract(r.boundary,'$.missionTurnId') AS missionTurnId,
  json_extract(k.state,'$.runnerId') AS attemptId,
  json_extract(k.state,'$.process') AS process,
  json_extract(k.state,'$.turnPending') AS turnPending,
  json_extract(k.state,'$.completed') AS providerCompleted,
  json_extract(k.state,'$.providerFailed') AS providerFailed,
  json_extract(k.state,'$.terminalCode') AS terminalCode,
  json_extract(k.state,'$.settled.status') AS settledStatus,
  ${progressProjection} AS progressJson
  FROM runs r LEFT JOIN run_checkpoints k ON k.run_id=r.id
  LEFT JOIN run_progress p ON p.run_id=r.id WHERE r.id=?`

export const canonicalWaitSql = `SELECT EXISTS(SELECT 1 FROM runs r
  WHERE r.id=? AND r.cancel_requested_at IS NULL AND r.recovery_pending=0 AND r.workspace_cleaned_at IS NULL
    AND ((r.status='waiting' AND EXISTS(SELECT 1 FROM durable_wakeups w WHERE w.mission_run_id=r.id
      AND w.message_id=json_extract(r.boundary,'$.missionTurnId') AND w.wake_at>?))
    OR (r.status='succeeded' AND EXISTS(SELECT 1 FROM conversations c JOIN durable_wakeups w ON w.chat_id=c.id
      WHERE c.run_id=r.id AND c.lifecycle='active' AND c.paused=0 AND c.session_restart_requested=0
        AND w.message_id=json_extract(r.boundary,'$.chatExecution.messageId') AND w.wake_at>?)))) AS valid`

function identity(value) {
  return typeof value === 'string' && value.length > 0 && value.length <= 256
}

function timestamp(value, now) {
  return Number.isSafeInteger(value) && value >= 0 && value <= now
}

function sameProcess(a, b) {
  return a && b && Number.isSafeInteger(a.pid) && a.pid > 1 && a.pid === b.pid
    && identity(a.start) && a.start === b.start && identity(a.boot) && a.boot === b.boot
}

export function prepareRunProgress(db) {
  const supported = db.prepare('SELECT 1 FROM sqlite_schema WHERE type=\'table\' AND name=\'run_progress\'').get()
  // Do not backfill from unbound historical events. Old writers have no signal.
  const sql = supported ? runProgressSql : runProgressSql.replace(progressProjection, 'NULL').replace('LEFT JOIN run_progress p ON p.run_id=r.id', '')
  return db.prepare(sql)
}

export function decodeRunProgress(row) {
  const { process, progressJson, ...safe } = row
  return { ...safe, process: process ? JSON.parse(process) : null, progress: progressJson ? JSON.parse(progressJson) : null }
}

export function progressOccurrence(run) {
  return JSON.stringify([run.attemptId || run.process || null, run.activeMessageId || run.missionTurnId || null])
}

// Values are good / bad / unknown / expected. Only good is recovery evidence.
export function observeRunProgress(run, now, quietMs = 20 * 60000) {
  const unknown = { status: 'unknown', phase: 'unknown' }
  if (run.turnPending === 1 || run.turnPending === true)
    return { ...unknown, phase: 'pending' }

  const terminal = ['succeeded', 'failed', 'interrupted', 'cancelled'].includes(run.status)
  const turn = run.activeMessageId || run.missionTurnId
  if (terminal && identity(turn) && identity(run.attemptId) && timestamp(run.finishedAt, now)
    && Number.isSafeInteger(run.terminalCode) && run.process === null) {
    return { status: 'good', phase: 'terminal', at: run.finishedAt }
  }

  if (run.status === 'waiting' && Number.isSafeInteger(run.terminalCode) && run.process === null)
    return { status: run.validFutureWait === 1 || run.validFutureWait === true ? 'expected' : 'unknown', phase: 'waiting' }

  const p = run.progress
  const bound = p?.version === 1 && identity(p.executionId) && identity(turn) && p.turnId === turn
    && ((identity(run.attemptId) && p.runnerId === run.attemptId) || (!run.attemptId && sameProcess(p.process, run.process)))
  if (!bound) {
    if (run.validFutureWait === 1 || run.validFutureWait === true)
      return { status: 'expected', phase: 'waiting' }
    return unknown
  }

  if (!timestamp(p.startedAt, now) || !timestamp(p.phaseAt, now) || p.phaseAt < p.startedAt
    || !['preparing', 'provider', 'tool', 'finalizing'].includes(p.phase)) {
    return unknown
  }

  if (p.advancedAt !== null && (!timestamp(p.advancedAt, now) || p.advancedAt < p.startedAt))
    return unknown

  // Run status is applied only after fencing/settlement. A provider completion
  // flag or settled *intent* on a still-running run is never terminal settlement.
  if (terminal && run.terminalCode !== null && run.terminalCode !== undefined)
    return { status: 'good', phase: 'terminal', at: p.phaseAt }

  const finalizing = p.phase === 'finalizing' || run.providerCompleted === 1 || run.providerCompleted === true
    || run.providerFailed === 1 || run.providerFailed === true || (run.terminalCode !== null && run.terminalCode !== undefined)
    || run.settledStatus != null
  if (finalizing) {
    // completed=false cannot blanch a sticky provider-completion/finalization episode.
    return { status: now - p.phaseAt > quietMs ? 'bad' : 'unknown', phase: 'finalizing', at: p.phaseAt }
  }

  if (p.complete !== 1 && p.complete !== true)
    return { ...unknown, phase: p.phase }
  if (p.advancedAt === null)
    return { status: now - p.startedAt > quietMs ? 'bad' : 'unknown', phase: 'preparing', at: p.startedAt }
  if (p.phase === 'tool' && now - p.advancedAt > quietMs)
    return { status: 'expected', phase: 'quiet_tool', at: p.advancedAt }
  return { status: now - p.advancedAt > quietMs ? 'bad' : 'good', phase: p.phase, at: p.advancedAt }
}
