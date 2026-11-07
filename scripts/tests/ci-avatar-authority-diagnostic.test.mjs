import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { it, onTestFinished } from 'vitest'
import { parse } from 'yaml'
import {
  AVATAR_ARGS,
  AVATAR_FILTER,
  runAvatarDiagnostic,
  WORKER_BUILD_ARGS,
  workerIdentity,
} from '../ci-avatar-authority-diagnostic.mjs'
import {
  avatarDiagnosticPayload,
  avatarWorkerProof,
  BACKEND_ARGS,
  BACKEND_COMMAND,
  backendDiagnosticCommand,
} from '../ci-backend-durable.mjs'

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-avatar-diagnostic-'))
  onTestFinished(() => rmSync(root, { recursive: true, force: true }))
  const source = path.join(root, 'candidate')
  const output = path.join(root, 'evidence')
  const worker = path.join(source, 'target/debug/cairn')
  for (const name of ['target/debug', 'scripts', 'crates/installation/tests/fixtures', 'crates/installation/src/agent_avatars'])
    mkdirSync(path.join(source, name), { recursive: true })
  mkdirSync(output)
  for (const name of ['scripts/test-backend.mjs', 'crates/installation/tests/fixtures/plugin_grants.rs', 'crates/installation/src/agent_avatars/tests.rs', 'Cargo.lock'])
    writeFileSync(path.join(source, name), `immutable fixture ${name}`)
  const candidate = '3'.repeat(40)
  const calls = []
  const options = {
    source,
    output,
    candidate,
    env: { CAIRN_TEST_CODE_WORKER: '/unrelated/live/worker', CAIRN_TEST_CODE_WORKERS: 'private-other-worker', PATH: process.env.PATH },
    identify: (directory, revision) => {
      assert.equal(directory, source)
      assert.equal(revision, candidate)
      return { commit: candidate, tree: '4'.repeat(40) }
    },
    run: (command, args, settings) => {
      calls.push({ command, args, settings })
      if (command === 'cargo')
        writeFileSync(worker, '#!/bin/sh\nexit 0\n', { mode: 0o700 })
      return { status: 0 }
    },
  }
  return {
    source,
    output,
    worker,
    candidate,
    options,
    calls,
  }
}

it('keeps the default full command exact and admits only the fixed named diagnostic', () => {
  assert.deepEqual(backendDiagnosticCommand(undefined, '/observer'), { command: BACKEND_COMMAND, args: BACKEND_ARGS })
  assert.deepEqual(backendDiagnosticCommand('full', '/observer'), { command: BACKEND_COMMAND, args: BACKEND_ARGS })
  assert.deepEqual(backendDiagnosticCommand('avatar-authority', '/observer'), { command: 'node', args: ['/observer/scripts/ci-avatar-authority-diagnostic.mjs'] })
  for (const value of ['', null, 'avatar-authority; command', '../shell', ['avatar-authority']])
    assert.throws(() => backendDiagnosticCommand(value, '/observer'))
  assert.throws(() => backendDiagnosticCommand('avatar-authority', 'relative'))
})

it('builds the matching worker before one exact package/lib invocation and records closed provenance', () => {
  const f = fixture()
  assert.equal(runAvatarDiagnostic(f.options), 0)
  assert.deepEqual(f.calls.map(call => [call.command, call.args]), [['cargo', WORKER_BUILD_ARGS], ['node', AVATAR_ARGS]])
  assert.deepEqual(AVATAR_ARGS, ['scripts/test-backend.mjs', '--nextest', '-p', 'cairn-installation', '--lib', '--test-threads', '8', '--no-fail-fast', '--color', 'never', '--status-level', 'all', '-E', AVATAR_FILTER])
  assert.equal(AVATAR_FILTER, 'test(agent_avatars::tests::) | test(authorization::tests::) | test(authorization::jobs::tests::)')
  for (const call of f.calls) {
    assert.equal(call.settings.cwd, f.source)
    assert.equal(call.settings.env.CARGO_TARGET_DIR, path.join(f.source, 'target'))
    assert.equal(call.settings.env.CAIRN_TEST_CODE_WORKER, undefined)
    assert.equal(call.settings.env.CAIRN_TEST_CODE_WORKERS, undefined)
  }

  const proof = avatarWorkerProof(f.output, f.candidate)
  assert.equal(proof.state, 'built')
  assert.equal(proof.qualificationClaim, false)
  assert.equal(proof.iteration, 1)
  assert.deepEqual(proof.worker, { path: 'target/debug/cairn', ...workerIdentity(f.worker) })
  assert.equal(proof.source.commit, f.candidate)
})

it.each([7, 101, 124])('preserves worker-build failure %s without launching tests or retrying', (status) => {
  const f = fixture()
  f.options.run = (...args) => {
    f.calls.push(args)
    return { status }
  }

  assert.equal(runAvatarDiagnostic(f.options), status)
  assert.equal(f.calls.length, 1)
  assert.equal(JSON.parse(readFileSync(path.join(f.output, 'avatar-build.json'), 'utf8')).exitCode, status)
  assert.equal(existsSync(path.join(f.output, 'avatar-worker.json')), false)
})

it('preserves the selected test failure and never retries it', () => {
  const f = fixture()
  const run = f.options.run
  f.options.run = (...args) => args[0] === 'node' ? (f.calls.push({ command: 'node' }), { status: 101 }) : run(...args)
  assert.equal(runAvatarDiagnostic(f.options), 101)
  assert.equal(f.calls.length, 2)
  assert.equal(JSON.parse(readFileSync(path.join(f.output, 'avatar-result.json'), 'utf8')).exitCode, 101)
})

it('rejects unknown command completion, cross-targets and source or worker changes', () => {
  const f = fixture()
  assert.throws(() => runAvatarDiagnostic({ ...f.options, run: () => ({ status: null, signal: 'SIGKILL' }) }))
  assert.throws(() => runAvatarDiagnostic({ ...f.options, env: { CARGO_BUILD_TARGET: 'another-target' } }))
  assert.throws(() => runAvatarDiagnostic({
    ...f.options,
    identify: () => {
      throw new Error('source mismatch')
    },
  }))
  const run = f.options.run
  assert.throws(() => runAvatarDiagnostic({
    ...f.options,
    run: (...args) => {
      const result = run(...args)
      if (args[0] === 'node')
        writeFileSync(f.worker, '#!/bin/sh\nexit 9\n')
      return result
    },
  }), /changed/)
})

it('refuses linked or non-executable workers instead of using an installed fallback', () => {
  const f = fixture()
  writeFileSync(f.worker, 'not executable')
  assert.throws(() => workerIdentity(f.worker))
  const linked = path.join(f.source, 'linked-worker')
  symlinkSync(f.worker, linked)
  assert.throws(() => workerIdentity(linked))
})

it('requires built provenance for a successful final diagnostic and preserves non-green pending evidence', () => {
  const f = fixture()
  assert.throws(() => avatarDiagnosticPayload({ acquisition: 'finished', guardReturn: { code: 0 } }, f.output, f.candidate))
  const failed = avatarDiagnosticPayload({ acquisition: 'finished', guardReturn: { code: 101 } }, f.output, f.candidate)
  assert.equal(failed.avatarWorker.state, 'not-built')
  assert.equal(failed.guardReturn.code, 101)
  assert.equal(avatarDiagnosticPayload({ acquisition: 'admitted-before-command' }, f.output, f.candidate).avatarWorker.state, 'not-built')
})

it('projects only fixed worker provenance and refuses malformed or mismatched evidence', () => {
  const f = fixture()
  assert.deepEqual(avatarWorkerProof(f.output, f.candidate), { state: 'not-built', qualificationClaim: false })
  assert.equal(runAvatarDiagnostic(f.options), 0)
  const file = path.join(f.output, 'avatar-worker.json')
  const proof = JSON.parse(readFileSync(file, 'utf8'))
  proof.rawSecret = 'private-value-must-not-be-projected'
  writeFileSync(file, JSON.stringify(proof))
  assert.ok(!JSON.stringify(avatarWorkerProof(f.output, f.candidate)).includes(proof.rawSecret))
  assert.throws(() => avatarWorkerProof(f.output, 'a'.repeat(40)))
  proof.worker.path = '/unrelated/worker'
  writeFileSync(file, JSON.stringify(proof))
  assert.throws(() => avatarWorkerProof(f.output, f.candidate))
})

it('exposes the selector only on the existing diagnostic action and retains normal/full workload', () => {
  const yaml = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  assert.deepEqual(yaml.on.workflow_dispatch.inputs['backend-diagnostic-selector'].options, ['full', 'avatar-authority'])
  assert.equal(yaml.on.workflow_dispatch.inputs['backend-diagnostic-selector'].default, 'full')
  const action = yaml.jobs.backend.steps.find(step => step.uses === './.github/actions/backend-durable-evidence')
  assert.equal(action.if, 'inputs.ci-diagnostic == \'backend\'')
  // eslint-disable-next-line no-template-curly-in-string -- exact allowlisted workflow expression
  assert.equal(action.env.BACKEND_DIAGNOSTIC_SELECTOR, '${{ inputs.backend-diagnostic-selector || \'full\' }}')
  const normal = yaml.jobs.backend.steps.find(step => step.name === 'Run backend tests across binaries with isolated credentials')
  assert.equal(normal.if, 'inputs.ci-diagnostic != \'backend\' && (inputs.backend-only != true || inputs.publication-measurement != true)')
  assert.ok(normal.run.includes('node scripts/test-backend.mjs --nextest --test-threads 8 --no-fail-fast --color never --status-level all'))
  assert.equal(yaml.jobs.backend['timeout-minutes'], 30)
})

it('refuses arbitrary entrypoint arguments before any build', () => {
  const result = spawnSync(process.execPath, ['scripts/ci-avatar-authority-diagnostic.mjs', 'untrusted-command'], {
    cwd: path.resolve(new URL('../..', import.meta.url).pathname),
    env: { PATH: process.env.PATH, BACKEND_DIAGNOSTIC_SELECTOR: 'avatar-authority' },
    encoding: 'utf8',
    timeout: 2000,
  })
  assert.equal(result.status, 125)
  assert.equal(result.stdout, '')
  assert.ok(!result.stderr.includes('untrusted-command'))
})
