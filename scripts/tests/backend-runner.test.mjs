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
const path = require('node:path');
const args = process.argv.slice(2);
const fixtures = process.env.CAIRN_TEST_WORKER_FIXTURES && JSON.parse(process.env.CAIRN_TEST_WORKER_FIXTURES);
const codeWorkers = process.env.CAIRN_TEST_CODE_WORKERS && JSON.parse(process.env.CAIRN_TEST_CODE_WORKERS);
const fixture = fixtures && fixtures[0].binary;
if (args[0] !== 'build' && fixture) for (const source of JSON.parse(fs.readFileSync(process.env.CAIRN_FIXTURE_SOURCE_LIST))) fs.writeFileSync(source, 'ordinary');
fs.appendFileSync(process.env.CAIRN_FIXTURE_CALLS, JSON.stringify({ args, worker: process.env.CAIRN_TEST_CODE_WORKER, codeWorkers: codeWorkers && codeWorkers.length > 1 ? codeWorkers : undefined, reused: process.env.CAIRN_TEST_REUSED_BUILD, auth: process.env.CAIRN_AUTH_SOCKET, fixture, fixtures, fixtureContents: fixture && fs.readFileSync(fixture, 'utf8'), allFixtureContents: fixtures && fixtures.map(item => fs.readFileSync(item.binary, 'utf8')) }) + '\\n');
if (args[0] === 'build' && args.includes('worker_fixture')) {
  if (Number(process.env.CAIRN_FIXTURE_BUILD_STATUS)) process.exit(Number(process.env.CAIRN_FIXTURE_BUILD_STATUS));
  const targets = args.flatMap((arg, index) => arg === '--target' ? [args[index + 1]] : arg.startsWith('--target=') ? [arg.slice(9)] : []);
  const sources = targets.length > 1 ? targets.map(target => path.join(process.env.CAIRN_FIXTURE_ROOT, target, 'debug', 'examples', 'worker_fixture')) : [process.env.CAIRN_FIXTURE_WORKER_SOURCE];
  for (const source of sources) {
    fs.mkdirSync(path.dirname(source), { recursive: true });
    fs.writeFileSync(source, 'instrumented');
    console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'worker_fixture' }, executable: source }));
  }
  fs.writeFileSync(process.env.CAIRN_FIXTURE_SOURCE_LIST, JSON.stringify(sources));
} else if (args[0] === 'build') {
  const targets = args.flatMap((arg, index) => arg === '--target' ? [args[index + 1]] : arg.startsWith('--target=') ? [arg.slice(9)] : []);
  const workers = targets.length > 1 ? targets.map(target => path.join(process.env.CAIRN_FIXTURE_ROOT, target, 'debug', 'cairn')) : ['/fixture/cairn'];
  for (const worker of workers) console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn' }, executable: worker }));
}
else process.exit(Number(process.env.CAIRN_FIXTURE_STATUS));
`)
    chmodSync(cargo, 0o755)
    const result = spawnSync(process.execPath, [launcher, ...args], {
      env: {
        ...process.env,
        PATH: `${directory}${path.delimiter}${process.env.PATH}`,
        CAIRN_FIXTURE_CALLS: calls,
        CAIRN_FIXTURE_WORKER_SOURCE: path.join(directory, 'worker_fixture'),
        CAIRN_FIXTURE_ROOT: directory,
        CAIRN_FIXTURE_SOURCE_LIST: path.join(directory, 'sources.json'),
        CAIRN_FIXTURE_STATUS: `${status}`,
        CAIRN_FIXTURE_BUILD_STATUS: `${buildStatus}`,
        CAIRN_AUTH_SOCKET: '/must-not-reach-tests',
      },
      encoding: 'utf8',
    })
    const recorded = existsSync(calls) ? readFileSync(calls, 'utf8').trim().split('\n').map(line => JSON.parse(line)) : []
    const fixtures = recorded.find(call => call.fixtures)?.fixtures
    return {
      status: result.status,
      stderr: result.stderr,
      calls: recorded,
      fixtureRemoved: fixtures ? fixtures.every(item => !existsSync(item.binary)) : undefined,
    }
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
    for (const spec of ['cairn-installation@0.52.3', 'cairn-installation:0.52.3', 'path+file:///fixture#cairn-installation@0.52.3']) {
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

  it('handles package and test globs and forwards Cargo build options', () => {
    const { calls } = invoke(['-p', 'cairn-*', '--test', 'work*', '--manifest-path', '/fixture/Cargo.toml', '--features=extra', '--no-default-features', '--ignore-rust-version', '-j4'])
    expect(calls[0].args).toEqual(expect.arrayContaining(['worker-fixture-hooks', '--manifest-path', '/fixture/Cargo.toml', '--features=extra', '--no-default-features', '--ignore-rust-version', '-j4']))
    expect(calls[1].args).toContain('cairn')
    expect(calls[2].fixtureContents).toBe('instrumented')
    const other = invoke(['-p', 'cairn-contract*', '--test', 'fixture'])
    expect(other.calls).toEqual([{ args: ['test', '--locked', '-p', 'cairn-contract*', '--test', 'fixture'] }])
    const otherTarget = invoke(['-p', 'cairn-installation', '--test', 'node*'])
    expect(otherTarget.calls).toEqual([{ args: ['test', '--locked', '-p', 'cairn-installation', '--test', 'node*'] }])
    const bracket = invoke(['-p', 'cairn-[ib]*', '--test', '[w]orker'])
    expect(bracket.calls[0].args).toContain('worker-fixture-hooks')
    expect(bracket.calls[1].args).toContain('cairn')
    const shortOptions = invoke(['-p', 'cairn-installation', '--test', 'worker', '-m', '/fixture/Cargo.toml', '-Fextra', '--all-features'])
    expect(shortOptions.calls[0].args).toEqual(expect.arrayContaining(['--manifest-path', '/fixture/Cargo.toml', '-Fextra', '--all-features']))
  })

  it('preserves a separate fixture for every Cargo target', () => {
    const result = invoke(['-p', 'cairn-installation', '--test', 'worker', '--target=first-target', '--target', 'second-target'])
    const fixtures = result.calls[1].fixtures
    expect(fixtures).toHaveLength(2)
    expect(fixtures.map(item => item.target)).toEqual(expect.arrayContaining([
      expect.stringContaining('/first-target/debug'),
      expect.stringContaining('/second-target/debug'),
    ]))
    expect(result.calls[1].allFixtureContents).toEqual(['instrumented', 'instrumented'])
    expect(result.fixtureRemoved).toBe(true)
    const workspace = invoke(['-p', 'cairn-*', '--tests', '--target', 'first-target', '--target=second-target'])
    expect(workspace.calls[2].codeWorkers).toHaveLength(2)
    expect(workspace.calls[2].codeWorkers.map(item => item.target)).toEqual(expect.arrayContaining([
      expect.stringContaining('/first-target/debug'),
      expect.stringContaining('/second-target/debug'),
    ]))
  })

  it('rejects reused binaries when the selected tests need a current fixture', () => {
    for (const option of ['--archive-file', '--binaries-metadata', '--cargo-metadata']) {
      for (const args of [[option, '/fixture/archive'], [`${option}=/fixture/archive`]]) {
        const result = invoke(['--nextest', ...args])
        expect(result.status).toBe(2)
        expect(result.stderr).toContain('cannot be paired safely with fixtures')
        expect(result.calls).toEqual([])
      }
    }

    const unrelated = invoke(['--nextest', '-p', 'cairn-contracts', '--archive-file', '/fixture/archive'])
    expect(unrelated.status).toBe(0)
    expect(unrelated.calls.at(-1).reused).toBe('1')
    const remapped = invoke(['--nextest', '--workspace-remap', '/fixture/source'])
    expect(remapped.calls.at(-1).reused).toBe('1')
    const ordinary = invoke(['--nextest'])
    expect(ordinary.calls.at(-1).reused).toBeUndefined()
  })
})
