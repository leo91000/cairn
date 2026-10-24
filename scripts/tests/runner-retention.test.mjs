import assert from 'node:assert/strict'
import { it } from 'vitest'
import { acknowledgeBeforeEviction } from '../../tests/runner-retention-smoke.mjs'

function journal({ continuous = false, refused = false } = {}) {
  let generation = 2
  let acknowledged = 1
  let lateWrite = true
  const captured = []
  const published = []
  const failure = new Error('synthetic publication acknowledgement refused')
  return {
    captured,
    published,
    failure,
    api: async path => ({
      json: async () => {
        if (path.endsWith('/snapshot')) {
          captured.push(generation)
          return { manifest: { generation } }
        }

        assert.ok(path.endsWith('/storage-status'))
        return { dirtyBytes: generation === acknowledged ? 0 : 28672 }
      },
    }),
    flushPublication: async (snapshot) => {
      if (!snapshot)
        return
      if (refused)
        throw failure
      acknowledged = snapshot.manifest.generation
      published.push(acknowledged)
      if (lateWrite || continuous) {
        generation++
        lateWrite = false
      }
    },
    until: async (operation, timeout) => {
      assert.equal(timeout, 10000)
      for (let attempt = 0; attempt < 3; attempt++) {
        if (await operation())
          return
      }

      throw new Error('synthetic journal never drained')
    },
  }
}

it('eviction publishes a queued write that lands after a snapshot acknowledgement', async () => {
  const state = journal()
  await acknowledgeBeforeEviction({ ...state, runId: 'synthetic', retention: 180 })
  assert.deepEqual(state.captured, [2, 3])
  assert.deepEqual(state.published, [2, 3])
})

it('a continuously dirty journal refuses eviction instead of bypassing its protection', async () => {
  const state = journal({ continuous: true })
  let admitted = false
  await assert.rejects(async () => {
    await acknowledgeBeforeEviction({ ...state, runId: 'synthetic', retention: 180 })
    admitted = true
  }, /never drained/)
  assert.equal(admitted, false)
})

it('failed publication acknowledgement stops the fixture before eviction', async () => {
  const state = journal({ refused: true })
  await assert.rejects(
    acknowledgeBeforeEviction({ ...state, runId: 'synthetic', retention: 180 }),
    error => error === state.failure,
  )
  assert.deepEqual(state.captured, [2])
  assert.deepEqual(state.published, [])
})

it('nonretained runs flush their completed publication without an eviction snapshot', async () => {
  const state = journal()
  let flushed = 0
  await acknowledgeBeforeEviction({
    ...state,
    runId: 'synthetic',
    retention: 0,
    flushPublication: async () => { flushed++ },
  })
  assert.equal(flushed, 1)
  assert.deepEqual(state.captured, [])
})
