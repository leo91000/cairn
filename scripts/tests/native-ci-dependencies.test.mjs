import { spawn } from 'node:child_process'
import {
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
import { parse } from 'yaml'

const root = new URL('../../', import.meta.url)
const workflow = parse(readFileSync(new URL('.github/workflows/ci.yaml', root), 'utf8'))
const setup = workflow.jobs.backend.steps.find(step => step.name === 'Install native test dependencies')

async function executeSetup(fault = '', {
  profile = 'native',
  closeOutput = false,
  blockedOutput = false,
  throwingMarker = false,
} = {}) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-native-dependencies-'))
  const history = path.join(directory, 'calls.jsonl')
  writeFileSync(history, '')
  const fixture = `#!${process.execPath}
const { spawn, spawnSync } = require('node:child_process')
const { appendFileSync } = require('node:fs')
const { basename } = require('node:path')
const { constants } = require('node:os')
const name = basename(process.argv[1])
const args = process.argv.slice(2)
const phase = name === 'sudo' ? 'entry' : args.includes('update') ? 'update' : 'install'
appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase, pid: process.pid, args }) + '\\n')
const fault = ${JSON.stringify(fault)}
if (phase === fault) {
  setInterval(() => {}, 1000)
} else if (phase === 'update' && ['hard-stall', 'orphan-stall', 'fail-descendant', 'success-descendant'].includes(fault)) {
  if (fault === 'hard-stall') process.on('SIGTERM', () => {})
  const child = spawn(process.execPath, ['-e', "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)"], { stdio: 'inherit' })
  appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase: 'descendant', pid: child.pid }) + '\\n')
  if (fault === 'fail-descendant') process.exit(42)
  if (fault === 'success-descendant') process.exit(0)
  setInterval(() => {}, 1000)
} else if (phase === 'update' || phase === 'install') {
  process.exit(fault === 'fail-' + phase ? 42 : 0)
} else {
  if (args[0] === '-n') args.shift()
  const command = args.shift()
  const result = spawnSync(command, args, { stdio: 'inherit' })
  process.exit(result.status ?? (128 + (constants.signals[result.signal] ?? 0)))
}
`
  for (const name of ['sudo', 'apt-get'])
    writeFileSync(path.join(directory, name), fixture, { mode: 0o700 })

  const started = Date.now()
  const fillOutput = `let full = false; const bytes = Buffer.alloc(64 * 1024); for (let i = 0; i < 256; i++) { try { writeSync(process.stdout.fd, bytes); } catch (error) { full = error.code === 'EAGAIN'; break; } } appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase: 'blocked-output', pid: process.pid, full }) + '\\n');`
  const driver = `import { writeSync, appendFileSync } from 'node:fs'; import { installNativeCiDependencies } from ${JSON.stringify(new URL('scripts/install-native-ci-dependencies.mjs', root).href)}; ${blockedOutput ? fillOutput : ''} process.exitCode = installNativeCiDependencies({ profile: ${JSON.stringify(profile)}, phaseSeconds: { update: 0.15, install: 0.15 }, entryAllowanceSeconds: 0.65, killAfterSeconds: 0.4${throwingMarker ? ', write: () => { throw new Error("fixture marker failure") }' : ''} });`
  const unsupportedProfile = !['native', 'android-network'].includes(profile)
  const useDriver = fault || closeOutput || blockedOutput || throwingMarker
  const androidSetup = workflow.jobs['android-direct'].steps.find(step => step.name === 'Install Android network test dependencies' || step.run?.includes('iproute2 iptables util-linux'))
  const command = unsupportedProfile || useDriver ? process.execPath : '/bin/bash'
  const args = unsupportedProfile
    ? [fileURLToPath(new URL('scripts/install-native-ci-dependencies.mjs', root)), '--profile', profile]
    : useDriver
      ? ['--input-type=module', '-e', driver]
      : ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', profile === 'android-network' ? androidSetup.run : setup.run]
  const child = spawn(command, args, {
    cwd: root,
    detached: true,
    env: { PATH: `${directory}${path.delimiter}${process.env.PATH}` },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let output = ''
  child.stdout.on('data', bytes => output += bytes)
  child.stderr.on('data', bytes => output += bytes)
  if (closeOutput)
    child.stdout.destroy()
  if (blockedOutput) {
    child.stdout.pause()
    child.on('exit', () => child.stdout.destroy())
  }

  let harnessKilled = false
  const timer = setTimeout(() => {
    harnessKilled = true
    try {
      process.kill(-child.pid, 'SIGKILL')
    }
    catch {
      // A faulty descendant may hold output after the direct child exited.
    }

    for (const line of readFileSync(history, 'utf8').trim().split('\n').filter(Boolean)) {
      try {
        process.kill(JSON.parse(line).pid, 'SIGKILL')
      }
      catch {
        // This fixture process already exited.
      }
    }
  }, 2000)

  try {
    const result = await new Promise(resolve => child.on('close', (status, signal) => resolve({ status, signal })))
    const calls = readFileSync(history, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line))
    const survivingFixturePids = calls.filter((call) => {
      try {
        return readFileSync(`/proc/${call.pid}/stat`, 'utf8').split(') ')[1][0] !== 'Z'
      }
      catch {
        return false
      }
    }).map(call => call.pid)
    return {
      ...result,
      output,
      calls,
      harnessKilled,
      survivingFixturePids,
      elapsedMs: Date.now() - started,
    }
  }
  finally {
    clearTimeout(timer)
    // Clean only this fixture's processes, including an interrupted test failure.
    for (const line of readFileSync(history, 'utf8').trim().split('\n').filter(Boolean)) {
      try {
        process.kill(JSON.parse(line).pid, 'SIGKILL')
      }
      catch {
        // The supervised fixture process already exited.
      }
    }

    rmSync(directory, { recursive: true, force: true })
  }
}

describe('native CI dependency setup', () => {
  it('installs the unchanged package set exactly once after a successful index refresh', async () => {
    const result = await executeSetup()
    expect(result.status).toBe(0)
    expect(result.harnessKilled).toBe(false)
    const apt = result.calls.filter(call => ['update', 'install'].includes(call.phase))
    expect(apt.map(call => call.phase)).toEqual(['update', 'install'])
    expect(apt[1].args.slice(-4)).toEqual(['libclang-dev', 'age', 'e2fsprogs', 'fakeroot'])
    expect(result.output).toContain('phase=update event=apt-start')
    expect(result.output).toContain('phase=install event=end code=0')
  })

  it('preserves the observed Android caller\'s exact closed package profile', async () => {
    const result = await executeSetup('', { profile: 'android-network' })
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(0)
    const apt = result.calls.filter(call => ['update', 'install'].includes(call.phase))
    expect(apt.map(call => call.phase)).toEqual(['update', 'install'])
    expect(apt[1].args.slice(apt[1].args.indexOf('install') + 1)).toEqual(['-y', '--no-install-recommends', 'iproute2', 'iptables', 'util-linux'])
    expect(result.output).toContain('profile=android-network')
  })

  it('bounds the observed Android profile through the same privileged supervisor', async () => {
    const result = await executeSetup('update', { profile: 'android-network' })
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(124)
    expect(result.output).toContain('profile=android-network')
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it.each(['arbitrary-packages', 'constructor'])('refuses unsupported profile %s before privilege entry', async (profile) => {
    const result = await executeSetup('', { profile })
    expect(result.status).toBe(2)
    expect(result.calls).toEqual([])
  })

  it.each(['entry', 'update', 'install'])('bounds and identifies a silent %s stall without starting later work', async (fault) => {
    const result = await executeSetup(fault)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(124)
    expect(result.output).toContain(`phase=${fault === 'install' ? 'install' : 'update'} event=start`)
    expect(result.output.includes('phase=update event=apt-start')).toBe(fault !== 'entry')
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(fault === 'install' ? 1 : 0)
    expect(result.elapsedMs).toBeLessThan(1800)
  })

  it.each(['update', 'install'])('preserves %s failure and does not retry it', async (phase) => {
    const result = await executeSetup(`fail-${phase}`)
    expect(result.status).toBe(42)
    expect(result.harnessKilled).toBe(false)
    expect(result.calls.filter(call => call.phase === phase)).toHaveLength(1)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(phase === 'install' ? 1 : 0)
  })

  it.each(['hard-stall', 'orphan-stall'])('kills owned descendants for %s, including after the leader exits', async (fault) => {
    const result = await executeSetup(fault)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(124)
    expect(result.output).toContain('phase=update event=apt-start')
    expect(result.calls.filter(call => call.phase === 'descendant')).toHaveLength(1)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it.each(['fail-descendant', 'success-descendant'])('cleans ordinary %s leader exit before returning its original status', async (fault) => {
    const result = await executeSetup(fault)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(fault === 'fail-descendant' ? 42 : 0)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(fault === 'fail-descendant' ? 0 : 1)
    expect(result.survivingFixturePids).toEqual([])
  })

  it.each(['fail-update', 'update', 'orphan-stall', 'fail-descendant'])('preserves %s classification with closed marker output', async (fault) => {
    const result = await executeSetup(fault, { closeOutput: true })
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(['fail-update', 'fail-descendant'].includes(fault) ? 42 : 124)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it.each(['fail-update', 'hard-stall', 'orphan-stall', 'fail-descendant'])('preserves %s and process cleanup when a marker writer throws', async (fault) => {
    const result = await executeSetup(fault, { throwingMarker: true })
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(['fail-update', 'fail-descendant'].includes(fault) ? 42 : 124)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it.each(['fail-update', 'update', 'fail-descendant'])('preserves %s under diagnostic output backpressure', async (fault) => {
    const result = await executeSetup(fault, { blockedOutput: true })
    expect(result.calls.find(call => call.phase === 'blocked-output')?.full).toBe(true)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(fault === 'update' ? 124 : 42)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it('uses the same bounded installer in all three native dependency callers', () => {
    for (const name of ['rust-check', 'backend', 'node-checks']) {
      const step = workflow.jobs[name].steps.find(step => step.name === 'Install native test dependencies')
      expect(step.run).toBe('node scripts/install-native-ci-dependencies.mjs')
      expect(step['timeout-minutes']).toBe(10)
    }

    const android = workflow.jobs['android-direct'].steps.find(step => step.name === 'Install Android network test dependencies')
    expect(android.run).toBe('node scripts/install-native-ci-dependencies.mjs --profile android-network')
    expect(android['timeout-minutes']).toBe(5)
    expect(workflow.jobs['android-direct']['timeout-minutes']).toBe(20)
  })
})
