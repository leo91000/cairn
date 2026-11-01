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
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const root = new URL('../../', import.meta.url)
const workflow = parse(readFileSync(new URL('.github/workflows/ci.yaml', root), 'utf8'))
const setup = workflow.jobs.backend.steps.find(step => step.name === 'Install native test dependencies')

async function executeSetup(fault = '', { closeOutput = false, throwingMarker = false } = {}) {
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
} else if (phase === 'update' && ['hard-stall', 'orphan-stall'].includes(fault)) {
  if (fault === 'hard-stall') process.on('SIGTERM', () => {})
  const child = spawn(process.execPath, ['-e', "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)"], { stdio: 'inherit' })
  appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase: 'descendant', pid: child.pid }) + '\\n')
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
  const driver = `import { installNativeCiDependencies } from ${JSON.stringify(new URL('scripts/install-native-ci-dependencies.mjs', root).href)}; process.exitCode = installNativeCiDependencies({ phaseSeconds: { update: 0.15, install: 0.15 }, entryAllowanceSeconds: 0.65, killAfterSeconds: 0.4${throwingMarker ? ', write: () => { throw new Error("fixture marker failure") }' : ''} });`
  const command = fault || closeOutput || throwingMarker ? process.execPath : '/bin/bash'
  const args = fault || closeOutput || throwingMarker
    ? ['--input-type=module', '-e', driver]
    : ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', setup.run]
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

  it.each(['fail-update', 'update', 'orphan-stall'])('preserves %s classification with closed marker output', async (fault) => {
    const result = await executeSetup(fault, { closeOutput: true })
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(fault === 'fail-update' ? 42 : 124)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it.each(['fail-update', 'hard-stall', 'orphan-stall'])('preserves %s and process cleanup when a marker writer throws', async (fault) => {
    const result = await executeSetup(fault, { throwingMarker: true })
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(fault === 'fail-update' ? 42 : 124)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
  })

  it('uses the same bounded installer in all three native dependency callers', () => {
    for (const name of ['rust-check', 'backend', 'node-checks']) {
      const step = workflow.jobs[name].steps.find(step => step.name === 'Install native test dependencies')
      expect(step.run).toBe('node scripts/install-native-ci-dependencies.mjs')
      expect(step['timeout-minutes']).toBe(10)
    }
  })
})
