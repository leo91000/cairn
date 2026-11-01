import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import test from 'node:test'
import { admissionWaitLimits, observeAdmissionWaits, readAdmissionOccurrence } from '../admission-wait.mjs'

const origin = 1700000000000

function capacity(id = 'run-one', extra = {}) {
  return {
    id,
    status: 'queued',
    waitClass: 'capacity',
    attemptId: 'attempt-one',
    activeMessageId: 'input-one',
    ...extra,
  }
}

const sample = (at, runs, complete = true) => ({ at, manager: { ok: true, data: { runs, runsComplete: complete } } })
const observation = (result, id = 'run-one') => result.observations.find(row => row.runId === id)

function mature(run = capacity()) {
  let result
  for (let minute = 0; minute <= 11; minute++)
    result = observeAdmissionWaits(sample(origin + minute * 60000, [run]), result?.state)
  assert.equal(observation(result, run.id).status, 'bad')
  return result
}

test('old conversation and latest unrelated event never supply the wait epoch', () => {
  const run = capacity('run-one', { createdAt: origin - 7 * 86400000, startedAt: origin - 600000, event: { createdAt: origin - 60000 } })
  const first = observeAdmissionWaits(sample(origin, [run]))
  assert.equal(observation(first).status, 'unknown')
  assert.equal(observation(first).observedSince, origin)
  const second = observeAdmissionWaits(sample(origin + 60000, [run]), first.state)
  assert.equal(observation(second).status, 'unknown')
  assert.equal(observation(second).observedSince, origin)
})

test('only fresh observed capacity samples mature, ignoring unrelated events', () => {
  let result
  for (let minute = 0; minute <= 10; minute++) {
    const run = capacity('run-one', { event: { createdAt: origin + minute * 60000 } })
    result = observeAdmissionWaits(sample(origin + minute * 60000, [run]), result?.state)
    assert.equal(observation(result).status, 'unknown')
  }

  result = observeAdmissionWaits(sample(origin + 11 * 60000, [capacity()]), result.state)
  assert.equal(observation(result).status, 'bad')
  assert.equal(observation(result).observedSince, origin)
})

test('a fresh run has its own age rather than inheriting another run wait', () => {
  const old = mature()
  const result = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity(), capacity('run-two')]), old.state)
  assert.equal(observation(result).status, 'bad')
  assert.equal(observation(result, 'run-two').status, 'unknown')
  assert.notEqual(observation(result).occurrence, observation(result, 'run-two').occurrence)
})

test('manager failure preserves episode identity but breaks duration and never proves recovery', () => {
  const old = mature()
  const unknown = observeAdmissionWaits({ at: origin + 12 * 60000, manager: { ok: false } }, old.state)
  assert.equal(observation(unknown).status, 'unknown')
  assert.equal(unknown.state.waits[0].since, null)
  const resumed = observeAdmissionWaits(sample(origin + 13 * 60000, [capacity()]), unknown.state)
  assert.equal(observation(resumed).status, 'unknown')
  assert.equal(observation(resumed).observedSince, origin + 13 * 60000)
  assert.equal(observation(resumed).occurrence, observation(old).occurrence)
})

test('missing rows in a partial inventory break age without rearming an episode', () => {
  const old = mature()
  const missing = observeAdmissionWaits(sample(origin + 12 * 60000, [], false), old.state)
  assert.equal(observation(missing).status, 'unknown')
  const resumed = observeAdmissionWaits(sample(origin + 13 * 60000, [capacity()], false), missing.state)
  assert.equal(observation(resumed).status, 'unknown')
  assert.equal(observation(resumed).occurrence, observation(old).occurrence)
  assert.equal(resumed.complete, false)
})

test('a currently present valid row still ages in a partial inventory', () => {
  const old = mature()
  const result = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity()], false), old.state)
  assert.equal(observation(result).status, 'bad')
})

test('complete inventory absence ends this capacity observation without claiming physical cleanup', () => {
  const old = mature()
  const result = observeAdmissionWaits(sample(origin + 12 * 60000, []), old.state)
  assert.equal(observation(result).status, 'good')
  assert.deepEqual(result.state.waits, [])
  assert.equal(observation(result).physicalFinalized, undefined)
})

test('account/recovery/other waits and known nonqueued statuses end only the capacity observation', () => {
  for (const extra of [
    { waitClass: 'account' },
    { waitClass: 'recovery' },
    { waitClass: 'other' },
    { status: 'running' },
    { status: 'waiting' },
    { status: 'succeeded' },
    { status: 'failed' },
  ]) {
    const old = mature()
    const result = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity('run-one', extra)]), old.state)
    assert.equal(observation(result).status, 'good')
    assert.deepEqual(result.state.waits, [])
    const again = observeAdmissionWaits(sample(origin + 13 * 60000, [capacity()]), result.state)
    assert.equal(observation(again).status, 'unknown')
    assert.notEqual(observation(again).occurrence, observation(old).occurrence)
  }
})

test('attempt and logical turn changes cannot inherit the old age', () => {
  for (const extra of [{ attemptId: 'attempt-two' }, { activeMessageId: 'input-two' }, { missionTurnId: 'turn-two' }]) {
    const old = mature()
    const result = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity('run-one', extra)]), old.state)
    assert.equal(observation(result).status, 'unknown')
    assert.notEqual(observation(result).occurrence, observation(old).occurrence)
  }
})

test('sampling gap resets duration while restart serialization preserves eligible history', () => {
  const old = mature()
  const restored = JSON.parse(JSON.stringify(old.state))
  const next = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity()]), restored)
  assert.equal(observation(next).status, 'bad')
  const late = observeAdmissionWaits(sample(origin + 15 * 60000 + 1, [capacity()]), next.state)
  assert.equal(observation(late).status, 'unknown')
  assert.equal(observation(late).occurrence, observation(old).occurrence)
})

test('clock rollback, future state and unsupported state never age a new observation', () => {
  const old = mature()
  for (const previous of [old.state, { ...old.state, version: 2 }, { ...old.state, waits: 'unsupported' }]) {
    const result = observeAdmissionWaits(sample(origin, [capacity()]), previous)
    assert.equal(observation(result).status, 'unknown')
    assert.equal(observation(result).observedSince, origin)
  }

  assert.deepEqual(observeAdmissionWaits({ at: Number.NaN }, old.state).observations, [])
})

test('malformed/duplicate identities and unsupported queued reasons stay unknown', () => {
  const old = mature()
  for (const runs of [
    [capacity('run-one', { status: 'unsupported' })],
    [capacity('run-one', { waitClass: null })],
    [capacity('run-one', { attemptId: null, activeMessageId: null })],
    [capacity('run-one', { attemptId: { unsafe: 'value' } })],
    [capacity(), capacity()],
  ]) {
    const result = observeAdmissionWaits(sample(origin + 12 * 60000, runs), old.state)
    assert.equal(observation(result).status, 'unknown')
    assert.equal(result.state.waits[0].since, null)
  }

  const missing = observeAdmissionWaits(sample(origin + 12 * 60000, [null]), old.state)
  assert.equal(missing.complete, false)
  assert.equal(observation(missing).status, 'unknown')
})

test('bounded state is independent of conversation content and cannot grow with historical runs', () => {
  const rows = Array.from({ length: admissionWaitLimits.entries + 1 }, (_, index) => capacity(`run-${index}`, { prompt: 'sensitive-sentinel' }))
  const first = observeAdmissionWaits(sample(origin, rows))
  assert.equal(first.complete, false)
  assert.equal(first.state.waits.length, admissionWaitLimits.entries)
  assert.equal(first.state.limited, true)
  const nextRows = rows.slice(0, 200).map((row, index) => ({ ...row, id: `next-${index}` }))
  const next = observeAdmissionWaits(sample(origin + 60000, nextRows, false), first.state)
  assert.equal(next.state.waits.length, admissionWaitLimits.entries)
  assert.equal(next.state.limited, true)
  assert(!JSON.stringify(next).includes('sensitive-sentinel'))
  assert(Buffer.byteLength(JSON.stringify(next.state)) < 100000)
})

test('bounded long technical identities survive persisted scope parsing', () => {
  const run = capacity('r'.repeat(256), { attemptId: 'a'.repeat(256), activeMessageId: 'b'.repeat(256), missionTurnId: 'c'.repeat(256) })
  const old = mature(run)
  assert.equal(observation(old, run.id).status, 'bad')
  assert(Buffer.byteLength(JSON.stringify(old.state)) < 2000)
})

test('rollback before the original timestamp resets only duration, not episode identity', () => {
  const old = mature()
  let result = observeAdmissionWaits(sample(origin - 60000, [capacity()]), old.state)
  assert.equal(observation(result).occurrence, observation(old).occurrence)
  assert.equal(observation(result).observedSince, origin - 60000)
  for (let minute = 0; minute <= 12; minute++)
    result = observeAdmissionWaits(sample(origin + minute * 60000, [capacity()]), result.state)
  assert.equal(observation(result).status, 'bad')
  assert.equal(observation(result).occurrence, observation(old).occurrence)
})

test('scope-checked retained identity restores history without borrowing its age', () => {
  const old = mature()
  const occurrence = observation(old).occurrence
  const previous = { ...old.state, waits: [], limited: true }
  const result = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity()]), previous, () => ({ occurrence }))
  assert.equal(observation(result).occurrence, occurrence)
  assert.equal(observation(result).observedSince, origin + 12 * 60000)
  assert.equal(observation(result).status, 'unknown')
  for (const retained of ['unsupported', JSON.stringify(['other-run', JSON.stringify(['attempt-one', 'input-one', null]), origin])]) {
    const unknown = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity()]), previous, () => ({ occurrence: retained }))
    assert.equal(observation(unknown).occurrence, undefined)
    assert.equal(observation(unknown).status, 'unknown')
  }
})

test('unreconciled lost history stays unknown until a known scope transition', () => {
  let result = {
    state: {
      version: 1,
      observedAt: origin,
      waits: [],
      limited: true,
    },
  }
  for (let minute = 1; minute <= 14; minute++) {
    result = observeAdmissionWaits(sample(origin + minute * 60000, [capacity()]), result.state)
    assert.equal(observation(result).status, 'unknown')
    assert.equal(observation(result).occurrence, undefined)
    assert.equal(result.state.limited, true)
  }

  result = observeAdmissionWaits(sample(origin + 15 * 60000, [capacity('run-one', { activeMessageId: 'next-input' })]), result.state)
  assert.equal(observation(result).firstObservedAt, origin + 15 * 60000)
  assert(readAdmissionOccurrence(observation(result).occurrence))
})

test('retained occurrence decoding is bounded, canonical and metadata-only', () => {
  const occurrence = observation(mature()).occurrence
  const decoded = readAdmissionOccurrence(occurrence)
  assert.equal(decoded.runId, 'run-one')
  assert.equal(decoded.scope, JSON.stringify(['attempt-one', 'input-one', null]))
  assert.equal(decoded.firstObservedAt, origin)
  assert.match(decoded.episodeId, /^[\w-]{36}$/)
  for (const value of [null, {}, 'x'.repeat(1201), '[]', '["run", "unsupported", 0]', JSON.stringify(['run', JSON.stringify([null, null, null]), 0])])
    assert.equal(readAdmissionOccurrence(value), null)
})

test('unsupported retained evidence and resolver failures cannot mint a fresh episode', () => {
  const failure = () => {
    throw new Error('not exported')
  }

  for (const callback of [() => 'unsupported', () => ({ fresh: false }), () => ({ unknown: true, occurrence: 'extra' }), failure]) {
    const result = observeAdmissionWaits(sample(origin, [capacity()]), undefined, callback)
    assert.equal(observation(result).status, 'unknown')
    assert.equal(observation(result).occurrence, undefined)
    assert(!JSON.stringify(result).includes('not exported'))
  }
})

test('a caller-confirmed end overrides still-retained helper identity without inheriting age', () => {
  const old = mature()
  const result = observeAdmissionWaits(sample(origin + 12 * 60000, [capacity()]), old.state, () => ({ fresh: true }))
  assert.equal(observation(result).status, 'unknown')
  assert.equal(observation(result).observedSince, origin + 12 * 60000)
  assert.notEqual(observation(result).occurrence, observation(old).occurrence)
})

test('a known-ended recurrence has a distinct nonce even when the clock repeats', () => {
  let count = 0
  const create = () => `00000000-0000-4000-8000-${(++count).toString(16).padStart(12, '0')}`
  const first = observeAdmissionWaits(sample(origin, [capacity()]), undefined, undefined, create)
  const ended = observeAdmissionWaits(sample(origin + 60000, [capacity('run-one', { waitClass: 'account' })]), first.state, undefined, create)
  const again = observeAdmissionWaits(sample(origin, [capacity()]), ended.state, () => ({ fresh: true }), create)
  assert.equal(observation(first).firstObservedAt, observation(again).firstObservedAt)
  assert.notEqual(observation(first).occurrence, observation(again).occurrence)
  assert.equal(count, 2)
  const restarted = observeAdmissionWaits(sample(origin + 60000, [capacity()]), JSON.parse(JSON.stringify(again.state)), undefined, create)
  assert.equal(observation(restarted).occurrence, observation(again).occurrence)
  assert.equal(count, 2)
})

test('unresolved legacy occurrence remains exact until a positive new episode', () => {
  const legacy = JSON.stringify(['run-one', JSON.stringify(['attempt-one', 'input-one', null]), origin])
  let count = 0
  const create = () => {
    count++
    return '00000000-0000-4000-8000-000000000001'
  }

  const restored = observeAdmissionWaits(sample(origin + 60000, [capacity()]), undefined, () => ({ occurrence: legacy }), create)
  assert.equal(observation(restored).occurrence, legacy)
  assert.equal(count, 0)
  const again = observeAdmissionWaits(sample(origin, [capacity()]), restored.state, () => ({ fresh: true }), create)
  assert.notEqual(observation(again).occurrence, legacy)
  assert.equal(count, 1)
})

test('unavailable or malformed fresh nonce never emits an invented episode', () => {
  const unavailable = () => {
    throw new Error('entropy unavailable')
  }

  for (const create of [() => 'unsupported', () => null, unavailable]) {
    const result = observeAdmissionWaits(sample(origin, [capacity()]), undefined, undefined, create)
    assert.equal(observation(result).status, 'unknown')
    assert.equal(observation(result).occurrence, undefined)
    assert(!JSON.stringify(result).includes('entropy unavailable'))
  }
})
