import { spawn } from 'node:child_process'
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { createServer } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const root = new URL('../../', import.meta.url)
const workflow = parse(readFileSync(new URL('.github/workflows/ci.yaml', root), 'utf8'))
const realSudo = process.env.CAIRN_TEST_REAL_SUDO === '1'
const setup = workflow.jobs.backend.steps.find(step => step.name === 'Install native test dependencies')

async function executeSetup(fault = '', {
  profile = 'native',
  closeOutput = false,
  blockedOutput = false,
  throwingMarker = false,
  cancelMarkerFailure = false,
  aptStartupDelayMs = 0,
  interruptSignal,
  interruptPhase = 'descendant',
} = {}) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-native-dependencies-'))
  const history = path.join(directory, 'calls.jsonl')
  writeFileSync(history, '')
  const closedControlPath = path.join(directory, 'closed-control.sock')
  const closedControl = fault === 'closed-control' ? createServer(socket => socket.end()) : undefined
  if (closedControl)
    await new Promise(resolve => closedControl.listen(closedControlPath, resolve))
  const recordSpawn = `import childProcess from 'node:child_process';import{syncBuiltinESMExports}from'node:module';import{appendFileSync}from'node:fs';if(['stalled-supervision','controller-crash'].includes(${JSON.stringify(fault)}))appendFileSync(${JSON.stringify(history)},JSON.stringify({phase:'controller',pid:process.pid})+'\\n');const original=childProcess.spawn;childProcess.spawn=function(command,...args){const child=original.call(this,command,...args);if(command==='apt-get')appendFileSync(${JSON.stringify(history)},JSON.stringify({phase:'apt-spawn',pid:process.pid})+'\\n');return child};const originalSync=childProcess.spawnSync;childProcess.spawnSync=function(command,...args){const result=originalSync.call(this,command,...args);if(${JSON.stringify(fault)}==='stalled-supervision'&&command==='/usr/bin/printf'&&args[0].some(value=>value.includes('event=apt-start')))Atomics.wait(new Int32Array(new SharedArrayBuffer(4)),0,0,5000);return result};syncBuiltinESMExports();`
  const fixture = `#!${process.execPath}
const { spawn, spawnSync } = require('node:child_process')
const { appendFileSync, readFileSync } = require('node:fs')
const { basename } = require('node:path')
const { constants } = require('node:os')
const name = basename(process.argv[1])
const args = process.argv.slice(2)
const phase = name === 'sudo' ? 'entry' : args.includes('update') ? 'update' : 'install'
appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase, pid: process.pid, uid: process.getuid(), args }) + '\\n')
if (phase !== 'entry' && ${aptStartupDelayMs} > 0)
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ${aptStartupDelayMs})
const fault = ${JSON.stringify(fault)}
if (phase === 'entry' && fault === 'delayed-entry') {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 150)
  appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase: 'entry-resumed', pid: process.pid }) + '\\n')
  args.splice(args.indexOf(process.execPath) + 1, 0, '--import', ${JSON.stringify(`data:text/javascript,${encodeURIComponent(recordSpawn)}`)})
}
if (phase === 'entry' && ['stalled-supervision', 'controller-crash'].includes(fault))
  args.splice(args.indexOf(process.execPath) + 1, 0, '--import', ${JSON.stringify(`data:text/javascript,${encodeURIComponent(recordSpawn)}`)})
if (phase === 'entry' && ['closure-marker-failure', 'fail-update-without-closure'].includes(fault))
  appendFileSync(args.at(-1) + '.update.closed', 'not a closure receipt')
if (phase === 'entry' && fault === 'closed-control')
  args[args.length - 1] = ${JSON.stringify(closedControlPath)}
if (phase === fault) {
  setInterval(() => {}, 1000)
} else if (phase === 'update' && ['hard-stall', 'orphan-stall', 'fail-descendant', 'success-descendant', 'stalled-supervision', 'owner-crash', 'controller-crash'].includes(fault)) {
  if (['hard-stall', 'stalled-supervision', 'owner-crash', 'controller-crash'].includes(fault)) process.on('SIGTERM', () => {})
  const child = spawn(process.execPath, ['-e', "process.on('SIGTERM', () => {}); setInterval(() => {}, 1000)"], { stdio: 'inherit' })
  appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase: 'descendant', pid: child.pid }) + '\\n')
  if (fault === 'owner-crash') process.kill(process.ppid, 'SIGKILL')
  if (fault === 'controller-crash') {
    const controller = readFileSync(${JSON.stringify(history)}, 'utf8').trim().split('\\n').map(JSON.parse).find(call => call.phase === 'controller')
    process.kill(controller.pid, 'SIGKILL')
  }
  if (fault === 'fail-descendant') process.exit(42)
  if (fault === 'success-descendant') process.exit(0)
  setInterval(() => {}, 1000)
} else if (phase === 'update' || phase === 'install') {
  process.exit(fault === 'fail-' + phase || (fault === 'fail-update-without-closure' && phase === 'update') ? 42 : 0)
} else {
  if (args[0] === '-n') args.shift()
  const command = args.shift()
  const result = ${realSudo ? `spawnSync('/usr/bin/sudo', ['-n', 'env', ${JSON.stringify(`PATH=${directory}${path.delimiter}${process.env.PATH}`)}, command, ...args], { stdio: 'inherit' })` : `spawnSync(command, args, { stdio: 'inherit' })`}
  process.exit(result.status ?? (128 + (constants.signals[result.signal] ?? 0)))
}
`
  for (const name of ['sudo', 'apt-get'])
    writeFileSync(path.join(directory, name), fixture, { mode: 0o700 })

  const started = Date.now()
  const fillOutput = `let full = false; const bytes = Buffer.alloc(64 * 1024); for (let i = 0; i < 256; i++) { try { writeSync(process.stdout.fd, bytes); } catch (error) { full = error.code === 'EAGAIN'; break; } } appendFileSync(${JSON.stringify(history)}, JSON.stringify({ phase: 'blocked-output', pid: process.pid, full }) + '\\n');`
  // Only deadline scenarios need a shortened work budget. Ordinary failures use
  // the production profile: startup and bounded marker writes may exceed 150 ms.
  // The independent two-second harness still rejects a stuck setup or cleanup.
  const exercisesDeadline = ['entry', 'update', 'install', 'hard-stall', 'orphan-stall', 'stalled-supervision'].includes(fault)
  const workSeconds = fault === 'controller-crash' ? 2 : interruptSignal && !cancelMarkerFailure ? 5 : exercisesDeadline ? 0.15 : undefined
  const phaseSeconds = workSeconds === undefined ? {} : { update: workSeconds, install: workSeconds }
  const cancelFault = cancelMarkerFailure ? `import fs from 'node:fs'; import { syncBuiltinESMExports } from 'node:module'; const mkdir = fs.mkdirSync; fs.mkdirSync = function(candidate, ...args) { if (candidate.endsWith('owner.sock.cancel')) throw new Error('fixture metadata refusal'); return mkdir.call(this, candidate, ...args); }; syncBuiltinESMExports();` : ''
  const driver = `${cancelFault} import { writeSync, appendFileSync } from 'node:fs'; import { installNativeCiDependencies } from ${JSON.stringify(new URL('scripts/install-native-ci-dependencies.mjs', root).href)}; ${blockedOutput ? fillOutput : ''} process.exitCode = await installNativeCiDependencies({ profile: ${JSON.stringify(profile)}, phaseSeconds: ${JSON.stringify(phaseSeconds)}, entryAllowanceSeconds: 0.65, killAfterSeconds: 0.4${throwingMarker ? ', write: () => { throw new Error("fixture marker failure") }' : ''} });`
  const unsupportedProfile = !['native', 'android-network'].includes(profile)
  const useDriver = fault || closeOutput || blockedOutput || throwingMarker
  const androidCommand = 'node scripts/install-native-ci-dependencies.mjs --profile android-network'
  const command = interruptSignal || unsupportedProfile || useDriver ? process.execPath : '/bin/bash'
  const args = (interruptSignal && interruptPhase === 'descendant') || unsupportedProfile
    ? [fileURLToPath(new URL('scripts/install-native-ci-dependencies.mjs', root)), '--profile', profile]
    : useDriver
      ? ['--input-type=module', '-e', driver]
      : ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', profile === 'android-network' ? androidCommand : setup.run]
  const child = spawn(command, args, {
    cwd: root,
    detached: true,
    env: { PATH: `${directory}${path.delimiter}${process.env.PATH}` },
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  let output = ''
  const liveFixturePids = () => readFileSync(history, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse).filter((call) => {
    try {
      return readFileSync(`/proc/${call.pid}/stat`, 'utf8').split(') ')[1][0] !== 'Z'
    }
    catch {
      return false
    }
  }).map(call => call.pid)
  let liveAtPublicExit
  child.on('exit', () => liveAtPublicExit = liveFixturePids())
  let signalSent = false
  const signalTimer = interruptSignal
    ? setInterval(() => {
        if (!signalSent && readFileSync(history, 'utf8').includes(`"phase":"${interruptPhase}"`)) {
          signalSent = true
          child.kill(interruptSignal)
        }
      }, 10)
    : undefined
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
    if (realSudo) {
      for (const call of calls.filter(call => ['update', 'install', 'descendant'].includes(call.phase))) {
        if (call.phase !== 'descendant')
          expect(call.uid).toBe(0)
      }
    }

    const controlDirectoriesRemaining = calls.filter(call => call.phase === 'entry')
      .map(call => call.args.at(-1))
      .filter(candidate => typeof candidate === 'string' && path.basename(candidate) === 'owner.sock')
      .map(candidate => path.dirname(candidate))
      .filter(candidate => existsSync(candidate))
    expect(controlDirectoriesRemaining).toEqual([])
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
      signalSent,
      liveAtPublicExit,
    }
  }
  finally {
    clearTimeout(timer)
    clearInterval(signalTimer)
    if (closedControl)
      await new Promise(resolve => closedControl.close(resolve))
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

  it.each(['native', 'android-network'])('cleans APT before public exit when its privileged controller stalls (%s)', async (profile) => {
    const result = await executeSetup('stalled-supervision', { profile })
    expect(result.status).toBe(137)
    expect(result.harnessKilled).toBe(false)
    expect(result.calls.filter(call => call.phase === 'descendant')).toHaveLength(1)
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.survivingFixturePids).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each(['native', 'android-network'])('awaits group cleanup after its privileged controller dies (%s)', async (profile) => {
    const result = await executeSetup('controller-crash', { profile })
    expect(result.status).toBe(137)
    expect(result.harnessKilled).toBe(false)
    expect(result.calls.filter(call => call.phase === 'controller')).toHaveLength(1)
    expect(result.calls.filter(call => call.phase === 'descendant')).toHaveLength(1)
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.survivingFixturePids).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each([['closure-marker-failure', 1], ['fail-update-without-closure', 42]])('never reports success without a closure receipt (%s)', async (fault, code) => {
    const result = await executeSetup(fault)
    expect(result.status).toBe(code)
    expect(result.harnessKilled).toBe(false)
    expect(result.output).toContain('event=cleanup-unknown')
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each(['native', 'android-network'])('cleans APT before public exit if its privileged shell is lost (%s)', async (profile) => {
    const result = await executeSetup('owner-crash', { profile })
    expect(result.status).toBe(137)
    expect(result.harnessKilled).toBe(false)
    expect(result.calls.filter(call => call.phase === 'descendant')).toHaveLength(1)
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.survivingFixturePids).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each([
    ['native', 'SIGINT', 130],
    ['native', 'SIGTERM', 143],
    ['android-network', 'SIGINT', 130],
    ['android-network', 'SIGTERM', 143],
  ])('settles stalled privileged supervision before public cancellation (%s, %s)', async (profile, interruptSignal, code) => {
    const result = await executeSetup('stalled-supervision', { profile, interruptSignal, interruptPhase: 'update' })
    expect(result.status).toBe(code)
    expect(result.signalSent).toBe(true)
    expect(result.harnessKilled).toBe(false)
    expect(result.calls.filter(call => call.phase === 'descendant')).toHaveLength(1)
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.survivingFixturePids).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each([['SIGINT', 130], ['SIGTERM', 143]])('retains owned cleanup if cancellation metadata fails (%s)', async (interruptSignal, code) => {
    const result = await executeSetup('stalled-supervision', { interruptSignal, interruptPhase: 'update', cancelMarkerFailure: true })
    expect(result.status).toBe(code)
    expect(result.signalSent).toBe(true)
    expect(result.harnessKilled).toBe(false)
    expect(result.output).toContain('phase=control event=cancel-marker-error')
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.survivingFixturePids).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each([['SIGINT', 130], ['SIGTERM', 143]])('finishes privileged cleanup before public %s exit', async (interruptSignal, code) => {
    const result = await executeSetup('hard-stall', { interruptSignal })
    expect(result.signalSent).toBe(true)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(code)
    expect(result.signal).toBeNull()
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.survivingFixturePids).toEqual([])
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
  })

  it.each([['SIGINT', 130], ['SIGTERM', 143]])('bounds public %s during stalled privilege entry', async (interruptSignal, code) => {
    const result = await executeSetup('entry', { interruptSignal, interruptPhase: 'entry' })
    expect(result.signalSent).toBe(true)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(code)
    expect(result.liveAtPublicExit).toEqual([])
    expect(result.calls.filter(call => call.phase === 'update')).toHaveLength(0)
  })

  it.each([['SIGINT', 130], ['SIGTERM', 143]])('refuses APT after delayed privilege entry resumes following %s', async (interruptSignal, code) => {
    const result = await executeSetup('delayed-entry', { interruptSignal, interruptPhase: 'entry' })
    expect(result.harnessKilled).toBe(false)
    expect(result.signalSent).toBe(true)
    expect(result.status).toBe(code)
    expect(result.calls.filter(call => call.phase === 'entry-resumed')).toHaveLength(1)
    expect(result.output).toContain('phase=update event=entry-ready')
    expect(result.calls.filter(call => call.phase === 'apt-spawn')).toHaveLength(0)
    expect(result.calls.filter(call => call.phase === 'update' || call.phase === 'install')).toHaveLength(0)
    expect(result.liveAtPublicExit).toEqual([])
  })

  it('refuses APT when the privileged start control channel is already closed', async () => {
    const result = await executeSetup('closed-control')
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(143)
    expect(result.calls.filter(call => call.phase === 'update' || call.phase === 'install')).toHaveLength(0)
    expect(result.liveAtPublicExit).toEqual([])
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

  it('preserves ordinary failure after startup outlasts the shortened timeout fixture budget', async () => {
    const result = await executeSetup('fail-descendant', { blockedOutput: true, aptStartupDelayMs: 200 })
    expect(result.calls.find(call => call.phase === 'blocked-output')?.full).toBe(true)
    expect(result.harnessKilled).toBe(false)
    expect(result.status).toBe(42)
    expect(result.calls.filter(call => call.phase === 'descendant')).toHaveLength(1)
    expect(result.calls.filter(call => call.phase === 'install')).toHaveLength(0)
    expect(result.survivingFixturePids).toEqual([])
    expect(result.liveAtPublicExit).toEqual([])
  })

  it('uses the same bounded installer in all native dependency callers with the same package set', () => {
    for (const name of ['rust-check', 'backend', 'cli-qualification', 'backend-migration-qualification', 'node-checks']) {
      const step = workflow.jobs[name].steps.find(step => step.name === 'Install native test dependencies')
      expect(step.run).toBe('node scripts/install-native-ci-dependencies.mjs')
      expect(step['timeout-minutes']).toBe(10)
    }

    const android = workflow.jobs['android-direct'].steps.find(step => step.name === 'Prepare the native Android network toolchain')
    expect(android.run).toBe('node scripts/prepare-network-tools.mjs android-direct')
    expect(workflow.jobs['android-direct'].steps.some(step => step.run?.includes('install-native-ci-dependencies'))).toBe(false)
    expect(workflow.jobs['android-direct']['timeout-minutes']).toBe(20)
  })
})
