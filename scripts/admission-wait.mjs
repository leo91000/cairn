// Metadata-only observation of a current admission wait. These timestamps are
// monitor observations, not the conversation's creation or a canonical wait start.
import { randomUUID } from 'node:crypto'

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

function episodeIdentity(value) {
  return typeof value === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value)
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
    if (!Array.isArray(parts) || ![3, 4].includes(parts.length) || !identity(parts[0])
      || !validScope(parts[1]) || !timestamp(parts[2]) || (parts.length === 4 && !episodeIdentity(parts[3]))
      || JSON.stringify(parts) !== value) {
      return null
    }

    return {
      runId: parts[0],
      scope: parts[1],
      firstObservedAt: parts[2],
      ...(parts.length === 4 ? { episodeId: parts[3] } : {}),
    }
  }
  catch {
    return null
  }
}

export function readAdmissionTransition(value) {
  if (typeof value !== 'string' || value.length > 1200)
    return null
  try {
    const parts = JSON.parse(value)
    if (!Array.isArray(parts) || parts.length !== 3 || !identity(parts[0])
      || !validScope(parts[1]) || parts[2] !== null || JSON.stringify(parts) !== value) {
      return null
    }

    return { runId: parts[0], scope: parts[1] }
  }
  catch {
    return null
  }
}

function previousWaits(previous) {
  if (previous == null)
    return { waits: new Map(), limited: false }
  if (!record(previous) || previous.version !== 1 || (previous.observedAt !== null && !timestamp(previous.observedAt))
    || (previous.limited !== undefined && typeof previous.limited !== 'boolean')
    || !Array.isArray(previous.waits)
    || previous.waits.length > admissionWaitLimits.entries) {
    return { waits: new Map(), limited: true }
  }

  const waits = new Map()
  for (const wait of previous.waits) {
    if (!record(wait) || !identity(wait.runId) || (!validScope(wait.scope) && !(wait.ended === true && wait.scope === null))
      || (wait.firstObservedAt !== null && !timestamp(wait.firstObservedAt))
      || (wait.lastObservedAt !== null && !timestamp(wait.lastObservedAt))
      || (wait.episodeId != null && (!episodeIdentity(wait.episodeId) || wait.firstObservedAt === null))
      || (wait.ended !== undefined && typeof wait.ended !== 'boolean')
      || (wait.ended === true && wait.transition !== true)
      || (wait.transition !== undefined && typeof wait.transition !== 'boolean')
      || (wait.transition === true && (wait.firstObservedAt !== null || wait.episodeId != null || wait.since !== null))
      || (wait.since !== null && (!timestamp(wait.since)
        || wait.firstObservedAt === null || !timestamp(wait.lastObservedAt) || previous.observedAt === null
        || wait.since > wait.lastObservedAt))
      || waits.has(wait.runId)) {
      return { waits: new Map(), limited: true }
    }

    waits.set(wait.runId, {
      runId: wait.runId,
      scope: wait.scope,
      firstObservedAt: wait.firstObservedAt,
      episodeId: wait.episodeId ?? null,
      transition: wait.transition === true,
      ended: wait.ended === true,
      lastObservedAt: wait.lastObservedAt,
      since: wait.since,
    })
  }

  return { waits, limited: previous.limited === true }
}

function endedWait(runId, currentScope, observedAt) {
  return {
    runId,
    scope: currentScope,
    firstObservedAt: null,
    episodeId: null,
    transition: true,
    ended: true,
    lastObservedAt: observedAt,
    since: null,
  }
}

// The caller supplies a fresh bounded manager probe on each invocation. An
// unavailable probe, omitted row or excessive sampling gap cannot age a wait.
// Unknown retains an episode's identity for deduplication, never certifies recovery.
export function observeAdmissionWaits(sample, previous, retainedEpisode, createEpisodeId = randomUUID) {
  const now = timestamp(sample?.at) ? sample.at : null
  const observations = new Map()
  const next = new Map()
  const history = previousWaits(previous)
  const prior = history.waits
  const manager = sample?.manager?.ok === true ? sample.manager.data : null
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
    const knownStatus = statuses.has(run.status)
    if (!knownStatus)
      complete = false

    const ended = knownStatus && (run.status !== 'queued' || ['account', 'recovery', 'other'].includes(run.waitClass))
    if (ended) {
      // A per-run positive end must survive unrelated bounded-history loss.
      // Its next episode is fresh even without an external identity callback.
      next.set(run.id, endedWait(run.id, scope(run) ?? prior.get(run.id)?.scope ?? null, now))
      observations.set(run.id, { id, runId: run.id, status: 'good' })
      continue
    }

    // Scope, gate and clock are independent facts. An unknown gate cannot
    // reinstall an older turn; only a known capacity gate can age the wait.
    const capacity = knownStatus && run.status === 'queued' && run.waitClass === 'capacity'
    const currentScope = scope(run)
    if (currentScope === null) {
      complete = false
      observations.set(run.id, { id, runId: run.id, status: 'unknown' })
      continue
    }

    const old = prior.get(run.id)
    const sameEpisode = old?.scope === currentScope
    let retained = old && (old.ended || old.transition || !sameEpisode) ? { fresh: true } : null
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
      // Identity reconciliation may fail independently of the validated scope.
      // Keep that scope so a later different turn cannot revive its predecessor.
      next.set(run.id, sameEpisode
        ? { ...old, since: null }
        : {
            runId: run.id,
            scope: currentScope,
            firstObservedAt: null,
            episodeId: null,
            transition: false,
            lastObservedAt: now,
            since: null,
          })
      observations.set(run.id, { id, runId: run.id, status: 'unknown' })
      limited = true
      continue
    }

    const fresh = retained?.fresh === true || (sameEpisode && old.transition)
    const positiveTransition = fresh || (!sameEpisode && !history.limited)
    let firstObservedAt = fresh ? null : recovered?.firstObservedAt ?? (sameEpisode ? old.firstObservedAt : null)
    if (firstObservedAt === null && positiveTransition && capacity)
      firstObservedAt = now
    let episodeId = fresh ? undefined : recovered ? recovered.episodeId ?? null : sameEpisode ? old.episodeId : undefined
    if (firstObservedAt !== null && episodeId === undefined) {
      try {
        episodeId = createEpisodeId()
      }
      catch {
        episodeId = undefined
      }

      if (!episodeIdentity(episodeId)) {
        // The scope was positively observed even when allocating its episode
        // failed. Keep that transition instead of reinstalling an ended scope.
        firstObservedAt = null
        episodeId = null
        limited = true
      }
    }

    const transition = firstObservedAt === null && positiveTransition
    const continuous = capacity && !fresh && now !== null && firstObservedAt !== null && sameEpisode && old.since !== null
      && episodeId === old.episodeId && firstObservedAt === old.firstObservedAt
      && timestamp(old.lastObservedAt) && now >= old.lastObservedAt && now - old.lastObservedAt <= admissionWaitLimits.gapMs
      && timestamp(previous.observedAt) && previous.observedAt <= now
    const wait = {
      runId: run.id,
      scope: currentScope,
      firstObservedAt,
      episodeId: episodeId ?? null,
      transition,
      lastObservedAt: now ?? (sameEpisode ? old.lastObservedAt : null),
      since: !capacity || firstObservedAt === null || now === null ? null : continuous ? old.since : now,
    }
    next.set(run.id, wait)
    if (firstObservedAt === null)
      limited = true
    observations.set(run.id, {
      id,
      runId: run.id,
      status: wait.since !== null && now - wait.since > admissionWaitLimits.quietMs ? 'bad' : 'unknown',
      ...(firstObservedAt !== null ? { occurrence: JSON.stringify([run.id, currentScope, firstObservedAt, ...(episodeId !== null ? [episodeId] : [])]) } : {}),
      ...(transition ? { transition: JSON.stringify([run.id, currentScope, null]) } : {}),
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
      if (next.size < admissionWaitLimits.entries)
        next.set(runId, endedWait(runId, old.scope, now))
      continue
    }

    observations.set(runId, { id: `admission-wait:${runId}`, runId, status: 'unknown' })
    if (next.size < admissionWaitLimits.entries)
      next.set(runId, { ...old, since: null })
    else if (!old.ended)
      limited = true
  }

  return {
    state: {
      version: 1,
      observedAt: now ?? (timestamp(previous?.observedAt) ? previous.observedAt : null),
      waits: [...next.values()],
      // Partial observations can resolve retained rows, never the unknown
      // identities already evicted from the bounded history.
      limited: limited || (history.limited && !complete),
    },
    observations: [...observations.values()],
    complete,
  }
}
