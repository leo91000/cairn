import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { observeRustCache } from '../ci-rust-cache-observation.mjs'

const environment = {
  GITHUB_ACTIONS: 'true',
  GITHUB_WORKSPACE: '/synthetic/workspace',
  RUNNER_TEMP: '/synthetic/temp',
  HOME: '/synthetic/home',
  GITHUB_SHA: '1'.repeat(40),
  GITHUB_JOB: 'node-checks',
  ImageOS: 'ubuntu24',
  ImageVersion: '20261031.1.0',
  GITHUB_RUN_ID: '123',
  GITHUB_RUN_ATTEMPT: '1',
  PRIVATE_TOKEN: 'secret-not-for-output',
}
const statistics = {
  bsize: 4096n,
  bavail: 10n,
  bfree: 11n,
  blocks: 20n,
  files: 1000n,
  ffree: 900n,
}

test('records bytes, inode capacity and directory aggregates without private paths or arbitrary environment', () => {
  const calls = []
  const result = observeRustCache('before-restore', environment, {
    now: () => 0,
    statfs: () => statistics,
    execute(command, args, options) {
      calls.push({ command, args, options })
      return '42\t/synthetic/private-name\n'
    },
  })
  assert.equal(result.filesystem.workspace.availableBytes, '40960')
  assert.equal(result.filesystem.workspace.freeInodes, '900')
  assert.equal(result.usage.targetDebug.allocatedBytes.value, '42')
  assert.equal(result.usage.excludedIncremental.inodes.value, '42')
  assert.equal(result.metadata.head, environment.GITHUB_SHA)
  assert.equal(calls.length, 15)
  assert.ok(calls.every(call => call.command === 'du' && call.args.includes('--') && call.options.timeout <= 15000))
  const serialized = JSON.stringify(result)
  assert.ok(!serialized.includes('secret-not-for-output'))
  assert.ok(!serialized.includes('/synthetic/'))
  assert.ok(!serialized.includes('private-name'))
})

test('bounds failed statfs, child errors and unexpected output to fixed error codes', () => {
  const result = observeRustCache('after-restore', environment, {
    now: () => 0,
    statfs() { throw Object.assign(new Error('private-path'), { code: 'secret-error-code' }) },
    execute() { throw Object.assign(new Error('private-stderr'), { code: 'ENOSPC' }) },
  })
  assert.deepEqual(result.filesystem.workspace, { readable: false, errorCode: 'unavailable' })
  assert.deepEqual(result.usage.targetDebug.allocatedBytes, { errorCode: 'ENOSPC' })
  assert.ok(!JSON.stringify(result).includes('private'))
  const invalid = observeRustCache('before-save', environment, {
    now: () => 0,
    statfs: () => statistics,
    execute: () => 'secret-not-a-number',
  })
  assert.deepEqual(invalid.usage.targetDebug.allocatedBytes, { errorCode: 'invalid_measurement' })
})

test('has one total observation budget across all directories', () => {
  let elapsed = 0
  let executions = 0
  const result = observeRustCache('before-save', environment, {
    now: () => elapsed,
    statfs: () => statistics,
    execute(_command, _args, options) {
      executions += 1
      assert.equal(options.timeout, 15000 - elapsed)
      elapsed += 5000
      return '10\tignored\n'
    },
  })
  assert.equal(executions, 3)
  assert.equal(result.elapsedMs, 15000)
  assert.deepEqual(result.usage.registryIndex.allocatedBytes, { errorCode: 'observation_budget_exhausted' })
})

test('rejects local execution, unknown phases and missing locations before filesystem or child work', () => {
  const dependencies = {
    statfs() { assert.fail('must not inspect filesystem') },
    execute() { assert.fail('must not execute') },
  }
  assert.throws(() => observeRustCache('unknown', environment, dependencies), /Unknown cache observation phase/)
  assert.throws(() => observeRustCache('before-restore', { ...environment, GITHUB_ACTIONS: 'false' }, dependencies), /only on a coordinated disposable CI host/)
  assert.throws(() => observeRustCache('before-restore', { ...environment, HOME: undefined }, dependencies), /absolute CI filesystem locations/)
  assert.throws(() => observeRustCache('before-restore', { ...environment, CARGO_HOME: 'relative' }, dependencies), /absolute Cargo location/)
})

test('CLI refuses a local probe and does not expose its environment', () => {
  const script = fileURLToPath(new URL('../ci-rust-cache-observation.mjs', import.meta.url))
  const result = spawnSync(process.execPath, [script, 'before-restore', '/not-written.json'], {
    encoding: 'utf8',
    env: { ...environment, GITHUB_ACTIONS: 'false' },
  })
  assert.equal(result.status, 1)
  assert.equal(result.stdout, '')
  assert.ok(!result.stderr.includes(environment.PRIVATE_TOKEN))
})
