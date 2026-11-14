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

const launcher = fileURLToPath(new URL('../test-native-namespace.mjs', import.meta.url))
const testName = 'storage::ublk::namespace::tests::isolated_namespace_qualification'
const retirementName = 'storage::ublk::tests::isolated_retirement_qualification'

function invoke(args, overrides = {}) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-namespace-runner-'))
  try {
    const callsFile = path.join(directory, 'calls.jsonl')
    const binary = path.join(directory, 'installation-test')
    const fixture = `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const name = path.basename(process.argv[1]);
const args = process.argv.slice(2);
fs.appendFileSync(process.env.CAIRN_NAMESPACE_CALLS, JSON.stringify({ name, args, auth: process.env.CAIRN_AUTH_SOCKET }) + '\\n');
if (name === 'cargo') {
  if (process.env.CAIRN_NAMESPACE_BUILD_FAILURE) process.exit(17);
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_beacon' }, executable: '/must-not-run' }));
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_installation' }, executable: null }));
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_installation' }, executable: process.env.CAIRN_NAMESPACE_BINARY }));
}
else if (name === 'installation-test') {
  if (!process.env.CAIRN_NAMESPACE_ABSENT) console.log('${testName}: test');
  if (!process.env.CAIRN_RETIREMENT_ABSENT) console.log('${retirementName}: test');
}
else process.exit(Number(process.env.CAIRN_NAMESPACE_QUALIFICATION_STATUS || '0'));
`
    for (const name of ['cargo', 'installation-test', 'sudo', 'unshare']) {
      const location = path.join(directory, name)
      writeFileSync(location, fixture)
      chmodSync(location, 0o755)
    }

    const result = spawnSync(process.execPath, [launcher, ...args], {
      encoding: 'utf8',
      timeout: 5000,
      env: {
        ...process.env,
        PATH: `${directory}${path.delimiter}${process.env.PATH}`,
        CAIRN_NAMESPACE_CALLS: callsFile,
        CAIRN_NAMESPACE_BINARY: binary,
        CAIRN_AUTH_SOCKET: '/must-not-reach-tests',
        ...overrides,
      },
    })
    const calls = existsSync(callsFile) ? readFileSync(callsFile, 'utf8').trim().split('\n').map(line => JSON.parse(line)) : []
    return { ...result, calls, binary }
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

describe('native namespace qualification launcher', () => {
  it('executes physical retirement only when explicitly selected on the privileged route', () => {
    const result = invoke(['--sudo', '--retirement'])
    expect(result.status).toBe(0)
    expect(result.calls.at(-1)).toEqual({ name: 'sudo', args: ['-n', 'unshare', '--mount', '--propagation', 'private', result.binary, '--exact', retirementName, '--ignored', '--nocapture'] })
    expect(invoke(['--retirement']).status).not.toBe(0)
    expect(invoke(['--sudo', '--retirement'], { CAIRN_RETIREMENT_ABSENT: '1' }).status).not.toBe(0)
  })

  it('retains the workspace feature graph and chooses the installation test among workspace artifacts', () => {
    const result = invoke(['--sudo'])
    expect(result.status).toBe(0)
    expect(result.calls).toEqual([
      { name: 'cargo', args: ['test', '--locked', '--workspace', '--lib', '--no-run', '--message-format=json'] },
      { name: 'installation-test', args: ['--list', '--ignored'] },
      { name: 'sudo', args: ['-n', 'unshare', '--mount', '--propagation', 'private', result.binary, '--exact', testName, '--ignored', '--nocapture'] },
    ])
  })

  it('retains the unprivileged namespace route', () => {
    const result = invoke([])
    expect(result.status).toBe(0)
    expect(result.calls.at(-1)).toEqual({
      name: 'unshare',
      args: ['--user', '--map-root-user', '--mount', '--propagation', 'private', result.binary, '--exact', testName, '--ignored', '--nocapture'],
    })
  })

  it('stops before namespace entry when compilation fails', () => {
    const result = invoke(['--sudo'], { CAIRN_NAMESPACE_BUILD_FAILURE: '1' })
    expect(result.status).toBe(17)
    expect(result.calls.map(call => call.name)).toEqual(['cargo'])
  })

  it('refuses namespace entry when the exact ignored case is absent', () => {
    const result = invoke(['--sudo'], { CAIRN_NAMESPACE_ABSENT: '1' })
    expect(result.status).not.toBe(0)
    expect(result.stderr).toContain('Namespace qualification test is absent')
    expect(result.calls.map(call => call.name)).toEqual(['cargo', 'installation-test'])
  })

  it('propagates qualification failure', () => {
    expect(invoke(['--sudo'], { CAIRN_NAMESPACE_QUALIFICATION_STATUS: '19' }).status).toBe(19)
  })
})
