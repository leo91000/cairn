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

const launcher = fileURLToPath(new URL('../ci/test-native-namespace.mjs', import.meta.url))
const blockStorageTests = [
  { target: 'cairn_block_storage', name: 'ublk::namespace::tests::isolated_namespace_qualification' },
  { target: 'cairn_block_storage', name: 'ublk::namespace::tests::recovery_template_drops_only_attested_private_copies' },
]
const installationTests = [
  { target: 'cairn_installation', name: 'native::block::holders::tests::retained_namespace_without_procfs_still_reports_the_device' },
  { target: 'cairn_installation', name: 'native::block::holders::tests::private_mount_outside_a_chroot_is_not_lost' },
  { target: 'cairn_installation', name: 'native::block::holders::tests::covered_namespace_root_requires_independent_superblock_inventory' },
  { target: 'cairn_installation', name: 'native::block::holders::tests::covered_network_namespace_does_not_block_filesystem_inventory' },
  { target: 'cairn_installation', name: 'native::systemd::directory_consumers::tests::persistent_mount_namespace_keeps_directory_ownership_without_tasks' },
]
const namespaceTests = [...blockStorageTests, ...installationTests]
const privilegedTests = [
  { target: 'cairn_installation', name: 'native::block::holders::tests::raw_block_device_descriptor_is_a_holder' },
  { target: 'cairn_installation', name: 'native::block::holders::tests::mapped_raw_block_node_is_a_holder_after_its_descriptor_closes' },
  { target: 'cairn_block_storage', name: 'ublk::observation::tests::encoded_absence_is_not_replaced_by_a_legacy_opcode_error' },
]

function invoke(args, overrides = {}) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-namespace-runner-'))
  try {
    const callsFile = path.join(directory, 'calls.jsonl')
    const binaries = {
      cairn_block_storage: path.join(directory, 'block-storage-test'),
      cairn_installation: path.join(directory, 'installation-test'),
    }
    const testsByBinary = {
      'block-storage-test': [...namespaceTests, ...privilegedTests].filter(test => test.target === 'cairn_block_storage').map(test => test.name),
      'installation-test': [...namespaceTests, ...privilegedTests].filter(test => test.target === 'cairn_installation').map(test => test.name),
    }
    const fixture = `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const name = path.basename(process.argv[1]);
const args = process.argv.slice(2);
fs.appendFileSync(process.env.CAIRN_NAMESPACE_CALLS, JSON.stringify({ name, args, auth: process.env.CAIRN_AUTH_SOCKET }) + '\\n');
if (name === 'cargo') {
  if (process.env.CAIRN_NAMESPACE_BUILD_FAILURE) process.exit(17);
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_beacon' }, executable: '/must-not-run' }));
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_block_storage' }, executable: null }));
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_block_storage' }, executable: process.env.CAIRN_NAMESPACE_BLOCK_BINARY }));
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_installation' }, executable: null }));
  console.log(JSON.stringify({ reason: 'compiler-artifact', target: { name: 'cairn_installation' }, executable: process.env.CAIRN_NAMESPACE_BINARY }));
}
else if (name === 'block-storage-test' || name === 'installation-test') {
  const tests = ${JSON.stringify(testsByBinary)}[name];
  if (!process.env.CAIRN_NAMESPACE_ABSENT) {
    for (const test of tests) {
      if (test !== process.env.CAIRN_NAMESPACE_MISSING_TEST) console.log(test + ': test');
    }
  }
}
else process.exit(Number(process.env.CAIRN_NAMESPACE_QUALIFICATION_STATUS || '0'));
`
    for (const name of ['cargo', 'block-storage-test', 'installation-test', 'sudo', 'unshare']) {
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
        CAIRN_NAMESPACE_BLOCK_BINARY: binaries.cairn_block_storage,
        CAIRN_NAMESPACE_BINARY: binaries.cairn_installation,
        CAIRN_AUTH_SOCKET: '/must-not-reach-tests',
        ...overrides,
      },
    })
    const calls = existsSync(callsFile) ? readFileSync(callsFile, 'utf8').trim().split('\n').map(line => JSON.parse(line)) : []
    return { ...result, calls, binaries }
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

describe('native namespace qualification launcher', () => {
  it('retains the workspace feature graph and chooses both owning test binaries among workspace artifacts', () => {
    const result = invoke(['--sudo'])
    expect(result.status).toBe(0)
    expect(result.calls).toEqual([
      { name: 'cargo', args: ['test', '--locked', '--workspace', '--lib', '--no-run', '--message-format=json'] },
      { name: 'block-storage-test', args: ['--list', '--ignored'] },
      { name: 'installation-test', args: ['--list', '--ignored'] },
      ...[...namespaceTests, ...privilegedTests].map(test => ({
        name: 'sudo',
        args: ['-n', 'unshare', '--mount', '--propagation', 'private', result.binaries[test.target], '--exact', test.name, '--ignored', '--nocapture'],
      })),
    ])
  })

  it('retains the unprivileged namespace route', () => {
    const result = invoke([])
    expect(result.status).toBe(0)
    expect(result.calls.slice(3)).toEqual(namespaceTests.map(test => ({
      name: 'unshare',
      args: ['--user', '--map-root-user', '--mount', '--propagation', 'private', result.binaries[test.target], '--exact', test.name, '--ignored', '--nocapture'],
    })))
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
    expect(result.calls.map(call => call.name)).toEqual(['cargo', 'block-storage-test'])
  })

  it('propagates qualification failure', () => {
    const result = invoke(['--sudo'], { CAIRN_NAMESPACE_QUALIFICATION_STATUS: '19' })
    expect(result.status).toBe(19)
    expect(result.calls.filter(call => call.name === 'sudo')).toHaveLength(1)
  })

  it('requires every selected case before entering a namespace', () => {
    for (const missing of privilegedTests) {
      const result = invoke(['--sudo'], { CAIRN_NAMESPACE_MISSING_TEST: missing.name })
      expect(result.status).not.toBe(0)
      expect(result.calls.filter(call => call.name === 'sudo')).toEqual([])
      expect(result.calls.filter(call => call.name === 'unshare')).toEqual([])
    }
  })
})
