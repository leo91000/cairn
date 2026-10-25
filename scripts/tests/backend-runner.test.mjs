import { spawnSync } from 'node:child_process'
import {
  chmodSync,
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

function invoke(args, status = 0) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-backend-runner-'))
  try {
    const cargo = path.join(directory, 'cargo')
    const calls = path.join(directory, 'calls.jsonl')
    writeFileSync(cargo, `#!/usr/bin/env node
const fs = require('node:fs');
const args = process.argv.slice(2);
const fixture = __dirname + '/manager-version';
fs.appendFileSync(process.env.CAIRN_FIXTURE_CALLS, JSON.stringify({ args, worker: process.env.CAIRN_TEST_CODE_WORKER, auth: process.env.CAIRN_AUTH_SOCKET }) + '\\n');
if (args[0] === 'build') {
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn' }, executable: '/fixture/cairn' }));
  if (args.includes('--example') && args.includes('worker_fixture')) {
    fs.writeFileSync(fixture, 'current');
    console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'worker_fixture' }, executable: '/fixture/worker_fixture' }));
  }
} else {
  if (args.includes('--test') && args.includes('slack') && fs.readFileSync(fixture, 'utf8') !== 'current') process.exit(97);
  process.exit(Number(process.env.CAIRN_FIXTURE_STATUS));
}
`)
    writeFileSync(path.join(directory, 'manager-version'), 'stale')
    chmodSync(cargo, 0o755)
    const result = spawnSync(process.execPath, [launcher, ...args], {
      env: {
        ...process.env,
        PATH: `${directory}${path.delimiter}${process.env.PATH}`,
        CAIRN_FIXTURE_CALLS: calls,
        CAIRN_FIXTURE_STATUS: `${status}`,
        CAIRN_AUTH_SOCKET: '/must-not-reach-tests',
      },
      encoding: 'utf8',
    })
    return { status: result.status, calls: readFileSync(calls, 'utf8').trim().split('\n').map(line => JSON.parse(line)) }
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
    expect(calls[0]).toEqual({ args: ['build', '--locked', '-p', 'cairn-installation', '--bin', 'cairn', '--example', 'worker_fixture', '--message-format=json'] })
    expect(calls[1]).toEqual({ args: ['nextest', 'run', '--locked', '--workspace', '--test-threads', '4', '--no-fail-fast'], worker: '/fixture/cairn' })
  })

  it('preserves the nextest failure exit code and package selection', () => {
    const { calls, status } = invoke(['--nextest', '-p', 'cairn-installation'], 100)
    expect(status).toBe(100)
    expect(calls).toEqual([
      { args: ['build', '--locked', '-p', 'cairn-installation', '--bin', 'cairn', '--example', 'worker_fixture', '--message-format=json'] },
      { args: ['nextest', 'run', '--locked', '-p', 'cairn-installation'], worker: '/fixture/cairn' },
    ])
  })

  it('rebuilds a stale native manager before the real Slack worker journey', () => {
    const { calls, status } = invoke(['-p', 'cairn-installation', '--test', 'slack', '--release', '--offline'])
    expect(status).toBe(0)
    expect(calls[0].args).toContain('--release')
    expect(calls[0].args).toContain('--offline')
    expect(calls.at(-1).args).toEqual(['test', '--locked', '-p', 'cairn-installation', '--test', 'slack', '--release', '--offline'])
  })
})
