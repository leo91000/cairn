// Validate the small counters-only wire contract before trusting any watermark.
const counters = ['records', 'bytes', 'errors', 'panics', 'leasesExpired', 'fencing', 'journalOpen', 'kernelIO', 'hungTask', 'unclassified', 'stdout', 'stderr', 'trace', 'debug', 'info', 'warnings']
const fields = new Set(['version', 'identity', 'window', 'coveredThrough', 'anchor', 'continuity', 'continuityUnknown', 'sourceChanged', 'complete', 'caughtUp', 'pages', 'attempts', 'counts', 'partialCounts', 'workBytes', 'limits', 'lastExitCode', 'eof', 'elapsedMs'])
const continuityKinds = new Set(['anchor_confirmed', 'anchor_missing', 'source_changed', 'unproven_bootstrap'])

export function validateLogCoverage(response, since, until) {
  const data = response?.data
  const lower = Date.parse(since)
  const upper = Date.parse(until)
  const through = Date.parse(data?.coveredThrough)
  const validCounts = value => value && counters.every(key => Number.isSafeInteger(value[key]) && value[key] >= 0)
  if (response?.ok !== true || !data || Object.keys(data).some(key => !fields.has(key)) || data.version !== 1 || !validCounts(data.counts) || !validCounts(data.partialCounts)
    || !Number.isFinite(lower) || !Number.isFinite(upper) || !Number.isFinite(through) || through < lower || through > upper
    || Date.parse(data.window?.since) !== lower || Date.parse(data.window?.until) !== upper
    || !Number.isSafeInteger(data.pages) || data.pages < 0 || data.pages > 8
    || !Number.isSafeInteger(data.attempts) || data.attempts < data.pages || data.attempts > 8
    || !continuityKinds.has(data.continuity) || data.continuityUnknown !== (data.continuity !== 'anchor_confirmed')
    || !data.identity || typeof data.identity.id !== 'string' || data.identity.id.length > 256
    || typeof data.identity.epoch !== 'string' || data.identity.epoch.length > 64
    || (data.anchor !== null && (!data.anchor || typeof data.anchor.at !== 'string' || !/^\d{1,22}$/.test(data.anchor.at)
      || typeof data.anchor.key !== 'string' || data.anchor.key.length > 300))
    || typeof data.complete !== 'boolean' || typeof data.caughtUp !== 'boolean' || typeof data.eof !== 'boolean'
    || (data.complete && (!data.caughtUp || !data.eof || data.lastExitCode !== 0 || through !== upper || data.pages === 0))
    || (through > lower && data.pages === 0)) {
    return { ok: false, reason: 'invalid_log_coverage' }
  }

  return {
    ok: true,
    data: {
      ...data,
      identity: { id: data.identity.id, epoch: data.identity.epoch },
      anchor: data.anchor && { at: data.anchor.at, key: data.anchor.key },
      counts: Object.fromEntries(counters.map(key => [key, data.counts[key]])),
      partialCounts: Object.fromEntries(counters.map(key => [key, data.partialCounts[key]])),
    },
  }
}

export function applyLogCoverage(previous, response, {
  since,
  until,
  at,
  acknowledgedAt = 0,
}) {
  const validated = validateLogCoverage(response, since, until)
  if (!validated.ok)
    return validated

  const data = validated.data
  const historicalGap = data.continuity !== 'anchor_confirmed'
  const gapKey = historicalGap ? JSON.stringify([data.continuity, data.identity, previous?.anchor?.key || null]) : previous?.gapKey
  const newGap = historicalGap && gapKey !== previous?.gapKey
  const gapSince = newGap ? at : previous?.gapSince || null
  const explicitlyAcknowledged = gapSince !== null && acknowledgedAt >= gapSince
    && data.complete && data.continuity === 'anchor_confirmed' && data.counts.unclassified === 0
  const gap = gapSince !== null && !explicitlyAcknowledged
  const state = {
    identity: data.identity,
    anchor: data.anchor,
    gapSince: gap ? gapSince : null,
    gapReason: gap ? newGap ? data.continuity : previous?.gapReason : null,
    gapKey: gap ? gapKey : null,
    coveredThrough: data.pages > 0 ? Date.parse(data.coveredThrough) : previous?.coveredThrough,
  }
  const signals = new Set(['errors', 'panics', 'leasesExpired', 'fencing', 'journalOpen', 'kernelIO', 'hungTask'])
  const observed = Object.fromEntries(counters.map(key => [key, data.counts[key] + (!data.complete && signals.has(key) ? data.partialCounts[key] : 0)]))
  return {
    ok: true,
    data: {
      ...data,
      ...observed,
      readComplete: data.complete,
      complete: data.complete && !gap && observed.unclassified === 0,
      gap,
      gapSince: state.gapSince,
      gapReason: state.gapReason,
      state,
    },
  }
}

export function pythonCollectorCommand(request) {
  return `python3 - --request '${JSON.stringify(request).replaceAll('\'', '\'\\\'\'')}'`
}
