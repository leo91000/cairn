import assert from 'node:assert/strict'
import filesystem from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, join } from 'node:path'
import test from 'node:test'
import { retainAlert } from '../health-evidence-retention.mjs'

const oldId = '2026-01-01T00-00-00-000Z'
const newId = '2026-01-01T01-00-00-000Z'

async function fixture(t) {
  const dir = await filesystem.mkdtemp(join(tmpdir(), 'cairn-evidence-retention-'))
  t.after(() => filesystem.rm(dir, { recursive: true, force: true }))
  const oldPath = join(dir, `alert-${oldId}.json`)
  await filesystem.writeFile(oldPath, '{"original":true}\n')
  return { dir, oldPath }
}

async function snapshot(dir) {
  const files = {}
  for (const entry of await filesystem.readdir(dir, { withFileTypes: true })) {
    if (entry.isFile())
      files[entry.name] = await filesystem.readFile(join(dir, entry.name), 'utf8')
  }

  return files
}

test('valid protection preserves absolute, relative, duplicate and missing references', async (t) => {
  const { dir, oldPath } = await fixture(t)
  const result = await retainAlert(dir, newId, { next: true }, {
    monitor: { maxAlertFiles: 1, maxAlertBytes: 4096 },
    retainedAlerts: [oldPath, basename(oldPath), 'missing-historical-evidence.json'],
    incidents: [{ evidencePaths: [oldPath] }],
  }, { incidentPath: oldPath })
  assert.equal(result.incidentPath, null)
  assert.equal(result.pressure, true)
  assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
  assert.equal((await filesystem.readdir(dir)).length, 1)
})

test('unknown protection containers or elements block every archive mutation', async (t) => {
  const cases = [
    ['object annotation', old => ({ retainedAlerts: [{ pinnedPath: old }] })],
    ['string container', old => ({ retainedAlerts: old })],
    ['object container', () => ({ retainedAlerts: {} })],
    ['number container', () => ({ retainedAlerts: 1 })],
    ['null container', () => ({ retainedAlerts: null })],
    ['null element', old => ({ retainedAlerts: [old, null] })],
    ['sparse paths', () => ({ retainedAlerts: Array.from({ length: 1 }) })],
    ['empty path', () => ({ retainedAlerts: [''] })],
    ['NUL path', () => ({ retainedAlerts: ['a\0b'] })],
    ['incidents string', () => ({ incidents: 'unknown' })],
    ['incidents object', () => ({ incidents: {} })],
    ['incidents null', () => ({ incidents: null })],
    ['invalid incident', () => ({ incidents: [null] })],
    ['nested path string', old => ({ incidents: [{ evidencePaths: old }] })],
    ['nested path object', () => ({ incidents: [{ evidencePaths: [{}] }] })],
  ]
  for (const [name, configure] of cases) {
    await t.test(name, async (t) => {
      const { dir, oldPath } = await fixture(t)
      const before = await snapshot(dir)
      const result = await retainAlert(dir, newId, { next: true }, {
        monitor: { maxAlertFiles: 1, maxAlertBytes: 4096 },
        ...configure(oldPath),
      })
      assert.equal(result.incidentPath, null)
      assert.equal(result.reason, 'invalid_protection')
      assert.deepEqual(await snapshot(dir), before)
      assert.ok(JSON.stringify(result).length < 400)
      assert.equal(JSON.stringify(result).includes(dir), false)
    })
  }
})

test('notification pins are validated before they are flattened', async (t) => {
  const cases = [
    { pending: 'invalid' },
    { pending: { incidentPath: {} } },
    { pending: { incidentPath: '' } },
    { related: 'invalid' },
    { related: [null] },
    { related: [{ incidentPath: {} }] },
  ]
  for (const entry of cases) {
    const { dir } = await fixture(t)
    const before = await snapshot(dir)
    const result = await retainAlert(dir, newId, {}, { monitor: { maxAlertFiles: 1 } }, entry.pending, entry.related)
    assert.equal(result.reason, 'invalid_protection')
    assert.deepEqual(await snapshot(dir), before)
  }
})

test('valid pending and related notification pins survive a full archive', async (t) => {
  const { dir, oldPath } = await fixture(t)
  const result = await retainAlert(dir, newId, {}, {
    monitor: { maxAlertFiles: 1 },
  }, null, [{ incidentPath: oldPath }, { incidentPath: null }, {}])
  assert.equal(result.incidentPath, null)
  assert.equal(result.reason, 'archive_budget')
  assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
})

test('missing optional configuration uses bounded defaults', async (t) => {
  const { dir } = await fixture(t)
  const result = await retainAlert(dir, newId, { next: true }, {})
  assert.equal(result.pressure, false)
  assert.equal(result.retainedFiles, 2)
  assert.deepEqual(JSON.parse(await filesystem.readFile(result.incidentPath, 'utf8')), { next: true })
})

test('count and byte budgets prune only unprotected old archives', async (t) => {
  const { dir, oldPath } = await fixture(t)
  const coordinator = {
    monitor: { maxAlertFiles: 3, maxAlertBytes: 600 },
    retainedAlerts: [oldPath],
  }
  for (let i = 1; i <= 20; i++) {
    const result = await retainAlert(dir, `2026-01-02T00-${i}-00-000Z`, { data: 'x'.repeat(100) }, coordinator)
    assert.equal(result.pressure, false)
    assert.ok(result.retainedFiles <= 3)
    assert.ok(result.retainedBytes <= 600)
  }

  assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
  assert.equal((await filesystem.readdir(dir)).length, 3)
})

test('invalid budgets and archive names cannot cause pruning', async (t) => {
  const invalid = [0, -1, 1.5, '1', null, Number.MAX_SAFE_INTEGER + 1]
  for (const budget of invalid) {
    for (const field of ['maxAlertFiles', 'maxAlertBytes']) {
      const { dir } = await fixture(t)
      const before = await snapshot(dir)
      const result = await retainAlert(dir, newId, {}, { monitor: { [field]: budget } })
      assert.equal(result.reason, 'invalid_budget')
      assert.deepEqual(await snapshot(dir), before)
    }
  }

  const { dir } = await fixture(t)
  const before = await snapshot(dir)
  const result = await retainAlert(dir, '../outside', {}, {})
  assert.equal(result.reason, 'invalid_archive')
  assert.deepEqual(await snapshot(dir), before)
})

test('an oversized or unserializable event does not discard older archives', async (t) => {
  const { dir } = await fixture(t)
  const before = await snapshot(dir)
  const oversized = await retainAlert(dir, newId, { value: 'x'.repeat(2048) }, { monitor: { maxAlertFiles: 1, maxAlertBytes: 100 } })
  assert.equal(oversized.reason, 'archive_budget')
  assert.deepEqual(await snapshot(dir), before)
  const circular = {}
  circular.self = circular
  const invalid = await retainAlert(dir, newId, circular, { monitor: { maxAlertFiles: 1 } })
  assert.equal(invalid.reason, 'archive_serialization')
  assert.deepEqual(await snapshot(dir), before)
  for (const value of [undefined, () => {}, Symbol('not JSON')]) {
    const result = await retainAlert(dir, newId, value, {})
    assert.equal(result.reason, 'archive_serialization')
    assert.deepEqual(await snapshot(dir), before)
  }
})

test('archive IO faults return a bounded fallback and preserve pinned evidence', async (t) => {
  for (const method of ['readdir', 'stat', 'writeFile', 'rename']) {
    await t.test(method, async (t) => {
      const { dir, oldPath } = await fixture(t)
      t.mock.method(filesystem, method, async () => {
        throw Object.assign(new Error('sensitive arbitrary filesystem detail'), { code: 'EACCES' })
      })
      const result = await retainAlert(dir, newId, {}, { retainedAlerts: [oldPath] })
      t.mock.restoreAll()
      assert.equal(result.incidentPath, null)
      assert.equal(result.reason, 'archive_io')
      assert.equal(result.code, 'EACCES')
      assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
      assert.equal(JSON.stringify(result).includes('sensitive'), false)
      assert.equal(JSON.stringify(result).includes(dir), false)
    })
  }
})

test('a write failure happens before any planned pruning', async (t) => {
  const { dir, oldPath } = await fixture(t)
  await filesystem.mkdir(join(dir, `alert-${newId}.json.tmp`))
  const result = await retainAlert(dir, newId, {}, { monitor: { maxAlertFiles: 1 } })
  assert.equal(result.reason, 'archive_io')
  assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
})

test('an existing staging file is neither overwritten nor cleaned up by another attempt', async (t) => {
  const { dir, oldPath } = await fixture(t)
  const staged = join(dir, `alert-${newId}.json.tmp`)
  await filesystem.writeFile(staged, 'unreviewed prior evidence')
  const before = await snapshot(dir)
  const result = await retainAlert(dir, newId, {}, { monitor: { maxAlertFiles: 1 } })
  assert.equal(result.reason, 'archive_io')
  assert.equal(result.code, 'EEXIST')
  assert.deepEqual(await snapshot(dir), before)
  assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
})

test('existing archive identifiers are not overwritten or pruned on replay', async (t) => {
  const { dir, oldPath } = await fixture(t)
  const before = await snapshot(dir)
  const result = await retainAlert(dir, oldId, { changed: true }, { monitor: { maxAlertFiles: 1 } })
  assert.equal(result.reason, 'archive_conflict')
  assert.equal(result.incidentPath, null)
  assert.deepEqual(await snapshot(dir), before)
  assert.equal(JSON.parse(await filesystem.readFile(oldPath, 'utf8')).original, true)
})
