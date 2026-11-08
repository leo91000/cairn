import assert from 'node:assert/strict'
import { test } from 'node:test'
import { commandBudget, ownershipClosed, validateProgress } from '../observe-header-evidence.mjs'

function completedJourney() {
  const events = []
  for (let trial = 0; trial < 100; trial++) {
    for (const method of ['filAndConversationShowOneHeaderAndTheConnectionSheet', 'switchingFromTheConnectionSheetSelectsTheOtherInstallation']) {
      const name = `${method}[trial=${trial}, cutout=${trial % 2 ? 48 : 0}dp]`
      events.push({ event: 'test-start', name }, { event: 'test-end', name, result: 'SUCCESS', errors: [] })
    }
  }
  events.push({ event: 'suite-end', result: 'SUCCESS', tests: 200, failed: 0, skipped: 0 })
  return events
}

test('only a complete fresh observation of every parameter can succeed', () => {
  const complete = validateProgress(completedJourney())
  assert.equal(complete.successful, true)
  assert.equal(complete.qualificationClaim, false)
  assert.equal(validateProgress([]).successful, false)
  assert.equal(validateProgress([{ event: 'suite-end', result: 'SUCCESS', tests: 200 }]).successful, false)
})

test('a lost final boundary preserves the unfinished test and remains non-green', () => {
  const events = completedJourney()
  const last = events.splice(-2)
  const result = validateProgress(events)
  assert.equal(result.complete, false)
  assert.deepEqual(result.pending, [last[0].name])
})

test('a duplicate parameter cannot stand in for the missing variant', () => {
  const events = completedJourney()
  events[2].name = events[0].name
  events[3].name = events[1].name
  assert.equal(validateProgress(events).complete, false)
})

test('completed failures and skipped tests cannot become successful observations', () => {
  for (const verdict of ['FAILURE', 'SKIPPED']) {
    const events = completedJourney()
    events[1].result = verdict
    const result = validateProgress(events)
    assert.equal(result.complete, true)
    assert.equal(result.successful, false)
    assert.equal(result.failures.length, 1)
  }
})

test('the work deadline includes setup time and reserves publication plus cleanup', () => {
  assert.equal(commandBudget({ startedAt: 0 }, 0), 1500)
  assert.equal(commandBudget({ startedAt: 0 }, 40 * 60000), 870)
  assert.equal(commandBudget({ startedAt: 0 }, 54 * 60000), 30)
  assert.throws(() => commandBudget({ startedAt: 0 }, 55 * 60000), /No diagnostic work budget/)
  assert.throws(() => commandBudget({ startedAt: 1000 }, 0), /Invalid job clock/)
})


test('unknown descendant closure cannot admit the second source', () => {
  const proof = { containmentReady: true, inventoryComplete: true, ownershipUncertain: false, cleanupUnknown: false }
  assert.equal(ownershipClosed(proof), true)
  assert.equal(ownershipClosed(undefined), false)
  assert.equal(ownershipClosed({ ...proof, cleanupUnknown: true }), false)
  assert.equal(ownershipClosed({ ...proof, inventoryComplete: false }), false)
  assert.equal(ownershipClosed({ ...proof, containmentReady: false }), false)
})
