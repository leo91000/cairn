import { spawnSync } from 'node:child_process'
import {
  chmodSync,
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const launcher = fileURLToPath(new URL('../test-backend.mjs', import.meta.url))

function invoke(args, status = 0, buildStatus = 0) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-backend-runner-'))
  try {
    const cargo = path.join(directory, 'cargo')
    const calls = path.join(directory, 'calls.jsonl')
    writeFileSync(cargo, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const fixture = process.env.CAIRN_TEST_WORKER_FIXTURE;
if (args[0] !== 'build' && fixture) fs.writeFileSync(process.env.CAIRN_FIXTURE_WORKER_SOURCE, 'ordinary');
fs.appendFileSync(process.env.CAIRN_FIXTURE_CALLS, JSON.stringify({ args, worker: process.env.CAIRN_TEST_CODE_WORKER, auth: process.env.CAIRN_AUTH_SOCKET, fixture, fixtureContents: fixture && fs.readFileSync(fixture, 'utf8') }) + '\\n');
if (args[0] === 'build' && args.includes('worker_fixture')) {
  if (Number(process.env.CAIRN_FIXTURE_BUILD_STATUS)) process.exit(Number(process.env.CAIRN_FIXTURE_BUILD_STATUS));
  fs.writeFileSync(process.env.CAIRN_FIXTURE_WORKER_SOURCE, 'instrumented');
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'worker_fixture' }, executable: process.env.CAIRN_FIXTURE_WORKER_SOURCE }));
} else if (args[0] === 'build') console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn' }, executable: '/fixture/cairn' }));
else process.exit(Number(process.env.CAIRN_FIXTURE_STATUS));
`)
    chmodSync(cargo, 0o755)
    const result = spawnSync(process.execPath, [launcher, ...args], {
      env: {
        ...process.env,
        PATH: `${directory}${path.delimiter}${process.env.PATH}`,
        CAIRN_FIXTURE_CALLS: calls,
        CAIRN_FIXTURE_WORKER_SOURCE: path.join(directory, 'worker_fixture'),
        CAIRN_FIXTURE_STATUS: `${status}`,
        CAIRN_FIXTURE_BUILD_STATUS: `${buildStatus}`,
        CAIRN_AUTH_SOCKET: '/must-not-reach-tests',
      },
      encoding: 'utf8',
    })
    const recorded = readFileSync(calls, 'utf8').trim().split('\n').map(line => JSON.parse(line))
    const fixture = recorded.find(call => call.fixture)?.fixture
    return { status: result.status, calls: recorded, fixtureRemoved: fixture ? !existsSync(fixture) : undefined }
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

describe('backend test launcher', () => {
  it('keeps cargo test and documentation checks as the default', () => {
    const { calls, status } = invoke(['--doc'])
    expect(status).toBe(0)
    expect(calls[0].args).toContain('build')
    expect(calls[1]).toEqual({ args: ['test', '--locked', '--workspace', '--doc'], worker: '/fixture/cairn' })
  })

  it('isolates credentials and builds the code worker with the nextest profile', () => {
    const { calls, status } = invoke(['--nextest', '--test-threads', '4', '--no-fail-fast'])
    expect(status).toBe(0)
    expect(calls[0]).toEqual({ args: ['build', '--locked', '-p', 'cairn-installation', '--example', 'worker_fixture', '--features', 'worker-fixture-hooks', '--message-format=json'] })
    expect(calls[1]).toMatchObject({ args: ['build', '--locked', '-p', 'cairn-installation', '--bin', 'cairn', '--message-format=json'], fixtureContents: 'instrumented' })
    expect(calls[2]).toMatchObject({ args: ['nextest', 'run', '--locked', '--workspace', '--test-threads', '4', '--no-fail-fast'], worker: '/fixture/cairn', fixtureContents: 'instrumented' })
  })

  it('preserves the nextest failure exit code and package selection', () => {
    const { calls, status } = invoke(['--nextest', '-p', 'cairn-installation'], 100)
    expect(status).toBe(100)
    expect(calls[0].args).toEqual(['build', '--locked', '-p', 'cairn-installation', '--example', 'worker_fixture', '--features', 'worker-fixture-hooks', '--message-format=json'])
    expect(calls[1]).toMatchObject({ args: ['nextest', 'run', '--locked', '-p', 'cairn-installation'], fixtureContents: 'instrumented' })
  })

  it('builds the instrumented worker fixture only when worker tests are selected', () => {
    const worker = invoke(['-p', 'cairn-installation', '--test', 'worker', '--target-dir', '/fixture/target'])
    expect(worker.calls[0].args).toEqual(['build', '--locked', '-p', 'cairn-installation', '--example', 'worker_fixture', '--features', 'worker-fixture-hooks', '--message-format=json', '--target-dir', '/fixture/target'])
    expect(worker.calls[1].args).toEqual(['test', '--locked', '-p', 'cairn-installation', '--test', 'worker', '--target-dir', '/fixture/target'])
    expect(worker.calls[1].fixtureContents).toBe('instrumented')
    expect(worker.fixtureRemoved).toBe(true)
    const other = invoke(['-p', 'cairn-installation', '--test', 'nodes'])
    expect(other.calls).toEqual([{ args: ['test', '--locked', '-p', 'cairn-installation', '--test', 'nodes'] }])
    const combined = invoke(['--package=cairn-installation', '--lib', '--test=worker'])
    expect(combined.calls[0].args).toContain('worker-fixture-hooks')
    const namedOther = invoke(['--package=cairn-installation', '--test=nodes'])
    expect(namedOther.calls).toEqual([{ args: ['test', '--locked', '--package=cairn-installation', '--test=nodes'] }])
    for (const selector of ['--tests', '--all-targets']) {
      const additive = invoke(['-p', 'cairn-installation', '--lib', selector])
      expect(additive.calls[0].args).toContain('worker-fixture-hooks')
      expect(additive.calls[1].fixtureContents).toBe('instrumented')
    }

    const short = invoke(['-pcairn-installation', '--test', 'worker'])
    expect(short.calls[0].args).toContain('worker-fixture-hooks')
    expect(short.calls[1].args).not.toContain('--workspace')
    const shortOther = invoke(['-pcairn-contracts', '--test', 'fixture'])
    expect(shortOther.calls).toEqual([{ args: ['test', '--locked', '-pcairn-contracts', '--test', 'fixture'] }])
    for (const spec of ['cairn-installation@0.52.3', 'path+file:///fixture#cairn-installation@0.52.3']) {
      const versioned = invoke(['-p', spec, '--test', 'worker'])
      expect(versioned.calls[0].args).toContain('worker-fixture-hooks')
    }

    const beacon = invoke(['--package=cairn-beacon@0.52.3', '--test', 'relay'])
    expect(beacon.calls[0].args).toContain('cairn')
  })

  it('propagates a fixture build failure without launching tests', () => {
    const result = invoke(['-p', 'cairn-installation', '--test', 'worker'], 0, 17)
    expect(result.status).toBe(17)
    expect(result.calls).toHaveLength(1)
    expect(result.calls[0].args).toContain('worker_fixture')
  })

  it('passes -r to both fixture and code-worker builds', () => {
    const { calls } = invoke(['--nextest', '--workspace', '-r'])
    expect(calls[0].args).toContain('-r')
    expect(calls[1].args).toContain('-r')
    expect(calls[2].fixtureContents).toBe('instrumented')
    expect(calls[2].args.filter(arg => arg === '--workspace')).toHaveLength(1)
  })

  it('distinguishes Nextest settings from Cargo build profile and jobs', () => {
    const { calls } = invoke(['--nextest', '--workspace', '--profile', 'ci', '--cargo-profile=dev', '--build-jobs', '2', '-j', '4'])
    for (const build of calls.slice(0, 2)) {
      expect(build.args).toContain('--profile=dev')
      expect(build.args).toContain('--jobs')
      expect(build.args).toContain('2')
      expect(build.args).not.toContain('ci')
      expect(build.args).not.toContain('4')
    }

    const cargo = invoke(['-p', 'cairn-installation', '--test', 'worker', '--profile', 'custom'])
    expect(cargo.calls[0].args.slice(-2)).toEqual(['--profile', 'custom'])
  })
})
