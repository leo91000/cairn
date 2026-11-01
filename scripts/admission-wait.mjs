// Metadata-only observation of a current admission wait. These timestamps are
// monitor observations, not the conversation's creation or a canonical wait start.
export const admissionWaitLimits = Object.freeze({ entries: 200, gapMs: 120000, quietMs: 600000 })

const statuses = new Set(['queued', 'running', 'waiting', 'succeeded', 'failed', 'interrupted', 'cancelled'])

function record(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function identity(value) {
  return typeof value === 'string' && /^[\w:-]{1,256}$/.test(value)
}

function timestamp(value) {
  return Number.isSafeInteger(value) && value >= 0
}

function scope(run) {
  const values = [run.attemptId, run.activeMessageId, run.missionTurnId]
  if (values.some(value => value != null && !identity(value)) || !values.some(identity))
    return null
  return JSON.stringify(values.map(value => value ?? null))
}

function validScope(value) {
  if (typeof value !== 'string' || value.length > 800)
    return false
  try {
    const values = JSON.parse(value)
    return Array.isArray(values) && values.length === 3 && values.some(identity)
      && values.every(value => value === null || identity(value)) && JSON.stringify(values) === value
  }
  catch {
    return false
  }
}

export function readAdmissionOccurrence(value) {
  if (typeof value !== 'string' || value.length > 1200)
    return null
  try {
    const parts = JSON.parse(value)
    if (!Array.isArray(parts) || parts.length !== 3 || !identity(parts[0])
      || !validScope(parts[1]) || !timestamp(parts[2]) || JSON.stringify(parts) !== value) {
      return null
    }

    return { runId: parts[0], scope: parts[1], firstObservedAt: parts[2] }
  }
  catch {
    return null
  }
}

function previousWaits(previous) {
  if (!record(previous) || previous.version !== 1 || !timestamp(previous.observedAt)
    || !Array.isArray(previous.waits)
    || previous.waits.length > admissionWaitLimits.entries) {
    return new Map()
  }

  const waits = new Map()
  for (const wait of previous.waits) {
    if (!record(wait) || !identity(wait.runId) || !validScope(wait.scope)
      || (wait.firstObservedAt !== null && !timestamp(wait.firstObservedAt)) || !timestamp(wait.lastObservedAt)
      || (wait.since !== null && (!timestamp(wait.since)
        || wait.firstObservedAt === null || wait.since > wait.lastObservedAt))
      || waits.has(wait.runId)) {
      return new Map()
    }

    waits.set(wait.runId, {
      runId: wait.runId,
      scope: wait.scope,
      firstObservedAt: wait.firstObservedAt,
      lastObservedAt: wait.lastObservedAt,
      since: wait.since,
    })
  }

  return waits
}

// The caller supplies a fresh bounded manager probe on each invocation. An
// unavailable probe, omitted row or excessive sampling gap cannot age a wait.
// Unknown retains an episode's identity for deduplication, never certifies recovery.
export function observeAdmissionWaits(sample, previous, retainedEpisode) {
  const now = sample?.at
  const observations = new Map()
  const next = new Map()
  if (!timestamp(now)) {
    return {
      state: {
        version: 1,
        observedAt: null,
        waits: [],
        limited: false,
      },
      observations: [],
      complete: false,
    }
  }

  const prior = previousWaits(previous)
  const manager = sample.manager?.ok === true ? sample.manager.data : null
  const rows = record(manager) && Array.isArray(manager.runs) ? manager.runs : []
  let complete = record(manager) && Array.isArray(manager.runs)
    && manager.runsComplete === true && rows.length <= admissionWaitLimits.entries
  let limited = rows.length > admissionWaitLimits.entries
  const seen = new Set()
  const duplicates = new Set()
  for (const run of rows.slice(0, admissionWaitLimits.entries)) {
    if (!record(run) || !identity(run.id)) {
      complete = false
      continue
    }

    if (seen.has(run.id)) {
      duplicates.add(run.id)
      complete = false
      continue
    }

    seen.add(run.id)
    const id = `admission-wait:${run.id}`
    if (!statuses.has(run.status)) {
      complete = false
      observations.set(run.id, { id, runId: run.id, status: 'unknown' })
      continue
    }

    if (run.status !== 'queued' || ['account', 'recovery', 'other'].includes(run.waitClass)) {
      observations.set(run.id, { id, runId: run.id, status: 'good' })
      continue
    }

    const currentScope = scope(run)
    if (run.waitClass !== 'capacity' || currentScope === null) {
      observations.set(run.id, { id, runId: run.id, status: 'unknown' })
      continue
    }

    const old = prior.get(run.id)
    const sameEpisode = old?.scope === currentScope
    let retained = old && !sameEpisode ? { fresh: true } : null
    if (retained === null && retainedEpisode !== undefined) {
      try {
        retained = typeof retainedEpisode === 'function' ? retainedEpisode(run.id, currentScope) : { unknown: true }
      }
      catch {
        retained = { unknown: true }
      }
    }

    if (retained != null && (!record(retained) || Object.keys(retained).length !== 1
      || !(typeof retained.occurrence === 'string' || retained.fresh === true || retained.unknown === true))) {
      retained = { unknown: true }
    }

    const recovered = readAdmissionOccurrence(retained?.occurrence)
    if (retained?.unknown || (retained?.occurrence !== undefined
      && (!recovered || recovered.runId !== run.id || recovered.scope !== currentScope))) {
      observations.set(run.id, { id, runId: run.id, status: 'unknown' })
      limited = true
      continue
    }

    let firstObservedAt = retained?.fresh === true ? now : recovered?.firstObservedAt ?? (sameEpisode ? old.firstObservedAt : null)
    if (firstObservedAt === null && (retained?.fresh === true || (!sameEpisode && !previous?.limited)))
      firstObservedAt = now
    const continuous = retained?.fresh !== true && firstObservedAt !== null && sameEpisode && old.since !== null
      && now >= old.lastObservedAt && now - old.lastObservedAt <= admissionWaitLimits.gapMs
      && previous.observedAt <= now
    const wait = {
      runId: run.id,
      scope: currentScope,
      firstObservedAt,
      lastObservedAt: now,
      since: firstObservedAt === null ? null : continuous ? old.since : now,
    }
    next.set(run.id, wait)
    if (firstObservedAt === null)
      limited = true
    observations.set(run.id, {
      id,
      runId: run.id,
      status: wait.since !== null && now - wait.since > admissionWaitLimits.quietMs ? 'bad' : 'unknown',
      ...(firstObservedAt !== null ? { occurrence: JSON.stringify([run.id, currentScope, firstObservedAt]) } : {}),
      observedSince: wait.since,
      firstObservedAt: wait.firstObservedAt,
    })
  }

  for (const runId of duplicates) {
    next.delete(runId)
    observations.set(runId, { id: `admission-wait:${runId}`, runId, status: 'unknown' })
  }

  for (const [runId, old] of prior) {
    const observation = observations.get(runId)
    if (next.has(runId) || observation?.status === 'good')
      continue
    if (!seen.has(runId) && complete) {
      observations.set(runId, { id: `admission-wait:${runId}`, runId, status: 'good' })
      continue
    }

    observations.set(runId, { id: `admission-wait:${runId}`, runId, status: 'unknown' })
    if (next.size < admissionWaitLimits.entries)
      next.set(runId, { ...old, since: null })
    else
      limited = true
  }

  return {
    state: {
      version: 1,
      observedAt: now,
      waits: [...next.values()],
      limited,
    },
    observations: [...observations.values()],
    complete,
  }
}
