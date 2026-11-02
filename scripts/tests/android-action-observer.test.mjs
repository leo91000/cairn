import { spawn, spawnSync } from 'node:child_process'
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'
import {
  classifyFdTarget,
  collectSnapshot,
  findActionNodeAncestor,
  MAX_RECORDED_PROCESS_COUNT,
  OBSERVER_HARD_STOP_MS,
  parseProcStat,
  SAMPLE_OFFSETS_MS,
  startObservation,
} from '../android-action-observer.mjs'

const repositoryRoot = fileURLToPath(new URL('../..', import.meta.url))
const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const pinnedEmulatorRunner = 'reactivecircus/android-emulator-runner@a421e43855164a8197daf9d8d40fe71c6996bb0d'

// Mirrors lib/script-parser.js at the pinned action commit (SHA-256 6d162ab4384c2ef9c261b5cc4dfb8947f69cab75b98fedb0ee34275619c170ff).
function parsePinnedActionScript(rawScript) {
  return rawScript.trim().split(/\r\n|\n|\r/).map(value => value.trim()).filter(value => !value.startsWith('#') && value.length > 0)
}

function runPinnedActionCommands(commands, environment) {
  for (const command of commands) {
    const result = spawnSync('sh', ['-c', command], {
      cwd: repositoryRoot,
      encoding: 'utf8',
      env: environment,
      timeout: 5000,
    })
    if (result.error)
      throw result.error
    if (result.status !== 0)
      return result.status
  }

  return 0
}

function readCallLog(filePath) {
  try {
    const value = readFileSync(filePath, 'utf8').trim()
    return value ? value.split('\n') : []
  }
  catch (error) {
    if (error.code === 'ENOENT')
      return []
    throw error
  }
}

function procStat(pid, comm, ppid, startTicks) {
  const fields = Array.from({ length: 20 }).fill('0')
  fields[0] = 'S'
  fields[1] = String(ppid)
  fields[19] = String(startTicks)
  return `${pid} (${comm}) ${fields.join(' ')}\n`
}

function createProcEntry(root, {
  pid,
  comm,
  ppid = 1,
  startTicks = pid,
  fds = [],
}) {
  const processDirectory = path.join(root, String(pid))
  const fdDirectory = path.join(processDirectory, 'fd')
  mkdirSync(fdDirectory, { recursive: true })
  writeFileSync(path.join(processDirectory, 'stat'), procStat(pid, comm, ppid, startTicks))
  writeFileSync(path.join(processDirectory, 'comm'), `${comm}\n`)
  for (const [fd, target] of fds) {
    symlinkSync(target, path.join(fdDirectory, String(fd)))
  }
}

function readProcessStartTicks(pid) {
  const stat = readFileSync(`/proc/${pid}/stat`, 'utf8')
  return parseProcStat(stat).startTicks
}

async function waitForFile(directory, name, timeoutMs = 3000) {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      if (readdirSync(directory, { withFileTypes: true }).some(entry => entry.name === name)) {
        return path.join(directory, name)
      }
    }
    catch (error) {
      if (error.code !== 'ENOENT') {
        throw error
      }
    }

    await delay(10)
  }

  throw new Error('observer snapshot did not appear')
}

describe('android action closure observer', () => {
  it('projects only whitelisted process metadata and aggregate descriptor kinds', () => {
    const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-observer-proc-'))
    const privateTarget = '/tmp/private-payload-path-987654321'
    try {
      mkdirSync(path.join(root, 'self'))
      createProcEntry(root, {
        pid: 200,
        comm: 'node',
        ppid: 100,
        startTicks: 7654321,
        fds: [
          [0, 'pipe:[10001]'],
          [1, 'socket:[10002]'],
          [2, 'anon_inode:[eventpoll]'],
          [3, '/dev/null'],
          [4, privateTarget],
        ],
      })
      createProcEntry(root, { pid: 201, comm: 'postgres', startTicks: 7654322 })
      createProcEntry(root, { pid: 202, comm: 'qemu-system-x86', startTicks: 7654323 })

      const snapshot = collectSnapshot({
        procRoot: root,
        actionNode: { pid: 200, startTicks: '7654321' },
        sampleIndex: 2,
        scheduledOffsetMs: 5000,
        elapsedMs: 5010,
      })
      const action = snapshot.processes.find(process => process.pid === 200)

      expect(snapshot.actionNode).toEqual({ pid: 200, startTicks: '7654321', status: 'present' })
      expect(snapshot.processes.map(process => process.comm)).toEqual(['node', 'qemu-system-x86'])
      expect(action).toMatchObject({
        pid: 200,
        ppid: 100,
        startTicks: '7654321',
        state: 'S',
        comm: 'node',
        fdTypes: {
          socket: 1,
          pipe: 1,
          anon_inode: 1,
          devnull: 1,
          other: 1,
        },
        stdio: { 0: 'pipe', 1: 'socket', 2: 'anon_inode' },
      })
      expect(JSON.stringify(snapshot)).not.toContain(privateTarget)
      expect(JSON.stringify(snapshot)).not.toContain('10001')
      expect(JSON.stringify(snapshot)).not.toContain('10002')
      expect(Object.keys(action).sort()).toEqual([
        'comm',
        'fdScan',
        'fdTypes',
        'pid',
        'ppid',
        'startTicks',
        'state',
        'stdio',
      ])
    }
    finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  it('parses command names containing parentheses and classifies only descriptor target kinds', () => {
    expect(parseProcStat(procStat(42, 'node (worker)', 7, 123456))).toMatchObject({
      pid: 42,
      comm: 'node (worker)',
      ppid: 7,
      startTicks: '123456',
      state: 'S',
    })
    expect(classifyFdTarget('socket:[2468]')).toBe('socket')
    expect(classifyFdTarget('pipe:[1357]')).toBe('pipe')
    expect(classifyFdTarget('anon_inode:[eventfd]')).toBe('anon_inode')
    expect(classifyFdTarget('/dev/null')).toBe('devnull')
    expect(classifyFdTarget('/private/path')).toBe('other')
  })

  it('selects the first Node ancestor above the sampler launcher', () => {
    const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-observer-ancestor-'))
    try {
      mkdirSync(path.join(root, 'self'))
      createProcEntry(root, {
        pid: process.pid,
        comm: 'node',
        ppid: 100,
        startTicks: 1234,
      })
      createProcEntry(root, {
        pid: 100,
        comm: 'bash',
        ppid: 101,
        startTicks: 2000,
      })
      createProcEntry(root, {
        pid: 101,
        comm: 'node',
        ppid: 1,
        startTicks: 3000,
      })

      expect(findActionNodeAncestor(root)).toEqual({ pid: 101, startTicks: '3000' })
    }
    finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  it('marks a reused action PID by its changed start ticks', () => {
    const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-observer-reused-pid-'))
    try {
      mkdirSync(path.join(root, 'self'))
      createProcEntry(root, {
        pid: 303,
        comm: 'node',
        ppid: 100,
        startTicks: 5678,
      })

      const snapshot = collectSnapshot({
        procRoot: root,
        actionNode: { pid: 303, startTicks: '1234' },
        sampleIndex: 0,
        scheduledOffsetMs: 0,
        elapsedMs: 0,
      })

      expect(snapshot.actionNode.status).toBe('reused')
      expect(snapshot.processes.find(process => process.pid === 303)).toMatchObject({
        pid: 303,
        startTicks: '5678',
        comm: 'node',
      })
    }
    finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  it('bounds process output and sampling to the 120-second window', () => {
    const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-observer-bound-'))
    try {
      mkdirSync(path.join(root, 'self'))
      const count = MAX_RECORDED_PROCESS_COUNT + 3
      for (let index = 1; index <= count; index += 1) {
        createProcEntry(root, { pid: 1000 + index, comm: 'node', startTicks: index })
      }

      const snapshot = collectSnapshot({
        procRoot: root,
        actionNode: { pid: 1001, startTicks: '1' },
        sampleIndex: 0,
        scheduledOffsetMs: 0,
        elapsedMs: 0,
      })

      expect(snapshot.processes).toHaveLength(MAX_RECORDED_PROCESS_COUNT)
      expect(snapshot.processScan).toBe('limit-reached')
      expect(SAMPLE_OFFSETS_MS).toEqual([0, 1000, 5000, 15000, 30000, 60000, 120000])
      expect(SAMPLE_OFFSETS_MS.at(-1)).toBe(OBSERVER_HARD_STOP_MS)
    }
    finally {
      rmSync(root, { recursive: true, force: true })
    }
  })

  it('spawns a detached sampler promptly and stops it after its owned target disappears', async () => {
    const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-observer-spawn-'))
    const outputDirectory = path.join(root, 'snapshots')
    const target = spawn(process.execPath, ['-e', 'process.title = "node"; setInterval(() => {}, 1000)'], { stdio: 'ignore' })
    let observer

    try {
      const targetStartTicks = readProcessStartTicks(target.pid)
      const startedAt = Date.now()
      observer = startObservation({
        outputDirectory,
        actionNode: { pid: target.pid, startTicks: targetStartTicks },
      })

      expect(observer.pid).toBeTruthy()
      expect(Date.now() - startedAt).toBeLessThan(500)
      expect(['0', '1', '2'].map(fd => classifyFdTarget(readlinkSync(`/proc/${observer.pid}/fd/${fd}`))))
        .toEqual(['devnull', 'devnull', 'devnull'])

      const firstSnapshotPath = await waitForFile(outputDirectory, 'sample-000.json')
      const snapshot = JSON.parse(readFileSync(firstSnapshotPath, 'utf8'))
      expect(snapshot.actionNode).toEqual({
        pid: target.pid,
        startTicks: targetStartTicks,
        status: 'present',
      })
      expect(snapshot.processes.some(process => process.pid === target.pid && process.comm === 'node')).toBe(true)
      expect(readFileSync(firstSnapshotPath).length).toBeGreaterThan(0)
      expect((await import('node:fs')).statSync(firstSnapshotPath).mode & 0o777).toBe(0o600)

      const observerExit = new Promise(resolve => observer.once('exit', resolve))
      target.kill('SIGTERM')
      await Promise.race([
        observerExit,
        delay(2500).then(() => {
          throw new Error('observer did not stop')
        }),
      ])
      expect(observer.exitCode).toBe(0)
      expect(readdirSync(outputDirectory).filter(name => /^sample-\d{3}\.json$/.test(name)).length).toBeGreaterThanOrEqual(1)
    }
    finally {
      target.kill('SIGKILL')
      observer?.kill('SIGTERM')
      rmSync(root, { recursive: true, force: true })
    }
  })

  it.each([0, 7, 23])('preserves Python exit code %s through the pinned parser and per-command sh caller', (pythonExitCode) => {
    const step = workflow.jobs['android-direct'].steps.find(candidate => candidate.name === 'Observe native responses, network restart and revocation')
    const script = step.with.script
    const temporaryDirectory = mkdtempSync(path.join(os.tmpdir(), 'cairn-observer-wrapper-'))
    const binDirectory = path.join(temporaryDirectory, 'bin')
    mkdirSync(binDirectory)

    try {
      const fakePython = path.join(binDirectory, 'python3')
      const fakeNode = path.join(binDirectory, 'node')
      const callLog = path.join(temporaryDirectory, 'calls.log')
      writeFileSync(fakePython, '#!/bin/sh\nprintf "python:%s\\n" "$ANDROID_SCENARIO" >> "$WRAPPER_CALL_LOG"\nexit "$FAKE_PYTHON_EXIT"\n')
      writeFileSync(fakeNode, '#!/bin/sh\ncase "$*" in\n  *android-action-observer.mjs*) printf "observer\\n" >> "$WRAPPER_CALL_LOG"; exit "$FAKE_OBSERVER_EXIT" ;;\n  *action-runner-sentinel*) printf "sentinel\\n" >> "$WRAPPER_CALL_LOG"; exit 0 ;;\n  *) printf "unexpected-node\\n" >> "$WRAPPER_CALL_LOG"; exit 0 ;;\nesac\n')
      chmodSync(fakePython, 0o700)
      chmodSync(fakeNode, 0o700)

      const commands = parsePinnedActionScript(script)
      const status = runPinnedActionCommands([...commands, 'node action-runner-sentinel'], {
        ...process.env,
        ANDROID_SCENARIO: 'same-lan',
        FAKE_OBSERVER_EXIT: '91',
        FAKE_PYTHON_EXIT: String(pythonExitCode),
        PATH: `${binDirectory}:/usr/bin:/bin`,
        WRAPPER_CALL_LOG: callLog,
      })
      const calls = readCallLog(callLog)

      expect(step.uses).toBe(pinnedEmulatorRunner)
      if (commands.length !== 1)
        expect(calls).toEqual([])
      expect(commands).toHaveLength(1)
      expect(status).toBe(pythonExitCode)
      expect(calls).toEqual(pythonExitCode === 0
        ? ['python:same-lan', 'observer', 'sentinel']
        : ['python:same-lan', 'observer'])
    }
    finally {
      rmSync(temporaryDirectory, { recursive: true, force: true })
    }
  })
})
