import { expect, it } from 'vitest'
import { applyLogCoverage, validateLogCoverage } from '../log-coverage.mjs'

const since = '2026-10-31T10:24:19.000Z'
const until = '2026-10-31T10:25:29.000Z'
const keys = ['records', 'bytes', 'errors', 'panics', 'leasesExpired', 'fencing', 'journalOpen', 'kernelIO', 'hungTask', 'unclassified', 'stdout', 'stderr', 'trace', 'debug', 'info', 'warnings']
const counts = () => Object.fromEntries(keys.map(key => [key, 0]))

function response(changes = {}) {
  return {
    ok: true,
    data: {
      version: 1,
      pageSpanNs: '70000000000',
      window: { since, until },
      coveredThrough: until,
      identity: { id: 'fixture', epoch: 'one' },
      anchor: { at: '1793442328999999999', key: 'anchor' },
      continuity: 'anchor_confirmed',
      continuityUnknown: false,
      complete: true,
      caughtUp: true,
      eof: true,
      lastExitCode: 0,
      pages: 1,
      attempts: 1,
      counts: counts(),
      partialCounts: counts(),
      ...changes,
    },
  }
}

it('never accepts a complete marker unsupported by completed EOF and exact coverage', () => {
  for (const changes of [{ pages: 0 }, { eof: false }, { lastExitCode: 2 }, { caughtUp: false }, { coveredThrough: since }])
    expect(validateLogCoverage(response(changes), since, until).ok).toBe(false)
})

it('credits the real completed old page without confusing it with catch-up', () => {
  const through = '2026-10-31T10:24:54.000Z'
  const result = applyLogCoverage(null, response({
    complete: false,
    caughtUp: false,
    eof: false,
    coveredThrough: through,
  }), { since, until, at: 1 })
  expect(result.ok).toBe(true)
  expect(result.data.complete).toBe(false)
  expect(result.data.state.coveredThrough).toBe(Date.parse(through))
  expect(result.data.state.backlog).toBe(true)
  expect(result.data.state.pageSpanNs).toBe('70000000000')
})

it('retains unknown history until an explicit acknowledgement and observed continuity', () => {
  const first = applyLogCoverage(null, response({ continuity: 'unproven_bootstrap', continuityUnknown: true }), { since, until, at: 10 })
  expect(first.data.gap).toBe(true)
  const later = applyLogCoverage(first.data.state, response(), { since, until, at: 20 })
  expect(later.data.complete).toBe(false)
  const accepted = applyLogCoverage(later.data.state, response(), {
    since,
    until,
    at: 30,
    acknowledgedAt: 10,
  })
  expect(accepted.data.complete).toBe(true)
  expect(accepted.data.gap).toBe(false)
})

it('an old acknowledgement cannot erase a newly detected source gap', () => {
  const previous = { gapSince: 10, gapKey: 'old', identity: { id: 'fixture', epoch: 'one' } }
  const missing = applyLogCoverage(previous, response({ continuity: 'anchor_missing', continuityUnknown: true }), {
    since,
    until,
    at: 20,
    acknowledgedAt: 10,
  })
  expect(missing.data.gapSince).toBe(20)
  const next = applyLogCoverage(missing.data.state, response(), {
    since,
    until,
    at: 30,
    acknowledgedAt: 10,
  })
  expect(next.data.gap).toBe(true)
})

it('unknown formatter and partial errors cannot become a healthy zero', () => {
  const result = applyLogCoverage(null, response({
    complete: false,
    caughtUp: false,
    counts: { ...counts(), unclassified: 2 },
    partialCounts: { ...counts(), errors: 3, panics: 1 },
  }), { since, until, at: 1 })
  expect(result.data.complete).toBe(false)
  expect(result.data.errors).toBe(3)
  expect(result.data.panics).toBe(1)
})
