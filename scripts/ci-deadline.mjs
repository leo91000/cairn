import { Buffer } from 'node:buffer'
import { spawn } from 'node:child_process'
import {
  chownSync,
  closeSync,
  mkdirSync,
  openSync,
  readdirSync,
  readSync,
  renameSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

function boundedRead(file, limit = 4096) {
  let fd
  try {
    fd = openSync(file, 'r')
    const bytes = Buffer.alloc(limit)
    return bytes.subarray(0, readSync(fd, bytes, 0, limit, 0)).toString()
  }
  catch {
    return undefined
  }
  finally {
    if (fd !== undefined)
      closeSync(fd)
  }
}

function identity(pid) {
  const text = boundedRead(`/proc/${pid}/stat`)
  if (!text)
    return undefined
  const fields = text.slice(text.lastIndexOf(')') + 2).trim().split(/\s+/)
  return {
    pid,
    parent: Number(fields[1]),
    session: Number(fields[3]),
    state: fields[0],
    cpuTicks: Number(fields[11]) + Number(fields[12]),
    startTicks: fields[19],
  }
}

function counters(text) {
  return Object.fromEntries((text ?? '').trim().split('\n').flatMap((line) => {
    const match = /^(\w+):?\s+(\d+)$/.exec(line)
    return match ? [[match[1], Number(match[2])]] : []
  }))
}

function processSample(item) {
  const root = `/proc/${item.pid}`
  const comm = boundedRead(`${root}/comm`, 128)?.trim()
  const role = ['cargo', 'rustc', 'rust-lld', 'ld', 'cc', 'cargo-nextest', 'node', 'python3', 'python'].includes(comm) ? comm : 'fixture-process'
  const membership = boundedRead(`${root}/cgroup`)?.split('\n').find(line => line.startsWith('0::'))?.slice(3)
  const group = membership && path.resolve('/sys/fs/cgroup', `.${membership}`)
  const stack = boundedRead(`${root}/stack`, 2048)
  const sample = {
    ...item,
    role,
    io: counters(boundedRead(`${root}/io`)),
    waitChannel: boundedRead(`${root}/wchan`, 128)?.trim(),
    kernelStack: stack?.split('\n').slice(0, 12),
    cgroup: group?.startsWith('/sys/fs/cgroup/')
      ? {
          cpu: counters(boundedRead(`${group}/cpu.stat`)),
          io: boundedRead(`${group}/io.stat`, 2048),
          memory: counters(boundedRead(`${group}/memory.events`)),
        }
      : undefined,
  }
  return identity(item.pid)?.startTicks === item.startTicks ? sample : { pid: item.pid, changedDuringObservation: true }
}

function backendProgress(output) {
  const phase = boundedRead(path.join(output, 'phase.json'))
  let observedPhase
  try {
    const value = JSON.parse(phase)
    if (['worker-fixture-build', 'code-worker-build', 'backend-tests'].includes(value.phase))
      observedPhase = value.phase
  }
  catch {}

  return observedPhase
}

function testProgress(output) {
  let fd
  try {
    const file = path.join(output, '..', 'backend-tests.log')
    const size = statSync(file).size
    fd = openSync(file, 'r')
    const buffer = Buffer.alloc(Math.min(size, 65536))
    const length = readSync(fd, buffer, 0, buffer.length, Math.max(0, size - buffer.length))
    const lines = buffer.subarray(0, length).toString().split('\n').slice(1)
    const statuses = {
      PASS: 0,
      FAIL: 0,
      SLOW: 0,
      TIMEOUT: 0,
    }
    for (const line of lines) {
      const status = /^\s*(PASS|FAIL|SLOW|TIMEOUT)\s/.exec(line)?.[1]
      if (status)
        statuses[status]++
    }

    return { logBytes: size, tailBytes: length, statusesInTail: statuses }
  }
  catch {
    return undefined
  }
  finally {
    if (fd !== undefined)
      closeSync(fd)
  }
}

// This is a diagnostic guard inside the existing job limit, never a new budget.
// Evidence and upload retain two minutes before GitHub can terminate the job.
export async function observeCommand({
  command,
  args,
  output,
  jobStartedAt,
  jobMinutes,
  reserveSeconds = 120,
  sampleMs = 30000,
  graceMs = 10000,
  closeGraceMs = 1000,
  inventoryLimit = 4096,
  commandEnv = process.env,
  beforeCommand,
  onCommandSettled,
  signal,
  stdout = process.stdout,
  stderr = process.stderr,
}) {
  const started = Date.now()
  if (!Number.isFinite(jobStartedAt) || jobStartedAt > started || started - jobStartedAt > 90 * 60000
    || !Number.isFinite(jobMinutes) || jobMinutes <= 0 || jobMinutes > 90 || reserveSeconds < 0
    || !Number.isInteger(inventoryLimit) || inventoryLimit < 1 || inventoryLimit > 4096) {
    throw new Error('Invalid bounded CI job clock')
  }

  mkdirSync(output, { recursive: true, mode: 0o700 })
  const evidenceOwner = statSync(output)
  const deadline = jobStartedAt + jobMinutes * 60000 - reserveSeconds * 1000
  const known = new Map()
  const samples = []
  let leader
  let timedOut = false
  let interrupted = false
  let inventoryComplete = true
  let ownershipUncertain = false
  let leaderExpected = false
  let cleanupUnknown = false
  let inventoryEntries = 0
  let containmentReady = false

  function owned() {
    const names = readdirSync('/proc').filter(name => /^\d+$/.test(name))
    inventoryEntries = names.length
    inventoryComplete &&= names.length <= inventoryLimit
    const rows = names.slice(0, inventoryLimit).flatMap(name => identity(Number(name)) ?? [])
    // The pinned leader is checked directly even if the bounded inventory is
    // incomplete. Missing inventory is explicitly UNKNOWN, never a green run.
    const pinned = leader && identity(leader.pid)
    if (pinned?.startTicks === leader?.startTicks && pinned && !rows.some(row => row.pid === pinned.pid))
      rows.push(pinned)
    if (leaderExpected && (!pinned || pinned.startTicks !== leader?.startTicks))
      ownershipUncertain = true
    const current = new Map(rows.map(row => [row.pid, row]))
    for (const [pid, start] of known) {
      if (current.get(pid)?.startTicks !== start)
        known.delete(pid)
    }

    // Keep the private session leader alive through final inspection. This
    // attributes even children reparented between samples without a PID/name
    // guess; an unrelated process cannot join this newly created session.
    if (leader && current.get(leader.pid)?.startTicks === leader.startTicks) {
      for (const row of rows) {
        if (row.session === leader.pid)
          known.set(row.pid, row.startTicks)
      }
    }

    const selected = new Map(rows.filter(row => known.get(row.pid) === row.startTicks).map(row => [row.pid, row]))
    let changed = true
    while (changed) {
      changed = false
      for (const row of rows) {
        if (!selected.has(row.pid) && selected.has(row.parent)) {
          selected.set(row.pid, row)
          known.set(row.pid, row.startTicks)
          changed = true
        }
      }
    }

    return [...selected.values()]
  }

  function persist(reason) {
    const rows = owned()
    const sample = {
      elapsedMs: Date.now() - started,
      reason,
      phase: backendProgress(output),
      processes: rows.slice(0, 16).map(processSample),
      processCount: rows.length,
      backend: testProgress(output),
      availableMemoryKiB: Number(/MemAvailable:\s+(\d+)/.exec(boundedRead('/proc/meminfo') ?? '')?.[1]) || undefined,
      load: boundedRead('/proc/loadavg', 128)?.trim(),
    }
    samples.push(sample)
    if (samples.length > 12)
      samples.shift()
    const proof = {
      jobStartedAt,
      deadline,
      timedOut,
      interrupted,
      inventoryComplete,
      inventoryEntries,
      inventoryLimit,
      ownershipUncertain,
      cleanupUnknown,
      containmentReady,
      containment: 'linux-child-subreaper',
      samples,
    }
    const file = path.join(output, 'latest.json')
    writeFileSync(`${file}.tmp`, `${JSON.stringify(proof)}\n`, { mode: 0o600 })
    if (process.getuid?.() === 0)
      chownSync(`${file}.tmp`, evidenceOwner.uid, evidenceOwner.gid)
    renameSync(`${file}.tmp`, file)
    console.log(`CI_DIAGNOSTIC ${JSON.stringify({
      reason,
      elapsedMs: sample.elapsedMs,
      phase: sample.phase,
      processCount: sample.processCount,
      timedOut,
    })}`)
    return rows
  }

  function signalOwned(signal) {
    // Match PID and kernel start time immediately before every signal. No
    // global process names, arbitrary cgroups or unowned host work are targeted.
    for (const row of owned().reverse()) {
      if (identity(row.pid)?.startTicks !== row.startTicks)
        continue
      try {
        process.kill(row.pid, signal)
      }
      catch (error) {
        if (error.code !== 'ESRCH')
          throw error
      }
    }
  }

  if (deadline <= started) {
    timedOut = true
    persist('no-time-before-evidence-reserve')
    return 124
  }

  if (beforeCommand) {
    persist('admitted-before-command')
    await beforeCommand()
  }

  if (signal?.aborted) {
    interrupted = true
    persist('external-termination')
    return 130
  }

  if (Date.now() >= deadline) {
    timedOut = true
    persist('no-time-before-evidence-reserve')
    return 124
  }

  const helper = fileURLToPath(new URL('./ci-owned-command.py', import.meta.url))
  const child = spawn('python3', [helper, command, ...args], {
    // Only this supervisor owns the public stdout/stderr descriptors. A child
    // stuck in kernel I/O cannot keep the workflow's tee open after cut-off.
    stdio: ['inherit', 'pipe', 'pipe', 'pipe', 'pipe'],
    detached: true,
    env: {
      ...commandEnv,
      CAIRN_CI_PHASE_FILE: path.resolve(output, 'phase.json'),
      CAIRN_CI_DIAGNOSTIC_OUTPUT: path.resolve(output),
    },
  })
  child.stdout.pipe(stdout, { end: false })
  child.stderr.pipe(stderr, { end: false })
  child.stdio[4].on('error', () => {
    if (!timedOut && !interrupted)
      ownershipUncertain = true
  })
  let spawnError
  let resolveReady
  let resolveCompleted
  const ready = new Promise(resolve => resolveReady = resolve)
  const completion = new Promise(resolve => resolveCompleted = resolve)
  const closed = new Promise((resolve) => {
    child.once('close', resolve)
    child.once('error', resolve)
  })
  child.once('error', (error) => {
    spawnError = error
    resolveReady(false)
    resolveCompleted({ code: 1 })
  })
  let reports = ''
  child.stdio[3].on('data', (bytes) => {
    reports += bytes.toString()
    if (reports.length > 4096) {
      ownershipUncertain = true
      resolveReady(false)
      return
    }

    while (reports.includes('\n')) {
      const index = reports.indexOf('\n')
      const line = reports.slice(0, index)
      reports = reports.slice(index + 1)
      let value
      try {
        value = JSON.parse(line)
      }
      catch {
        ownershipUncertain = true
        resolveReady(false)
        continue
      }

      if (value.type === 'subreaper-ready') {
        containmentReady = true
        resolveReady(true)
      }

      if (value.type === 'command-complete' && Number.isInteger(value.code))
        resolveCompleted({ code: value.code })
    }
  })
  child.once('exit', (code, signal) => {
    resolveReady(false)
    resolveCompleted({ code, signal })
  })
  const first = identity(child.pid)
  if (first) {
    leader = first
    known.set(first.pid, first.startTicks)
  }

  let readyTimer
  const didStart = await Promise.race([ready, new Promise(resolve => readyTimer = setTimeout(resolve, 1000, false))])
  clearTimeout(readyTimer)
  ownershipUncertain ||= !didStart || !containmentReady
  leaderExpected = true
  persist('started')
  let resolveStopped
  const stopped = new Promise(resolve => resolveStopped = resolve)

  function stop(reason) {
    if (timedOut || interrupted)
      return
    timedOut = reason === 'deadline'
    interrupted = !timedOut
    persist(reason)
    signalOwned('SIGTERM')
    resolveStopped({ code: 1 })
  }

  const onTerm = () => stop('external-termination')
  process.on('SIGTERM', onTerm)
  process.on('SIGINT', onTerm)
  signal?.addEventListener('abort', onTerm, { once: true })
  if (signal?.aborted)
    onTerm()
  const interval = setInterval(() => {
    persist('periodic')
    if (!inventoryComplete || ownershipUncertain)
      stop('ownership-incomplete')
  }, sampleMs)
  const timer = setTimeout(stop, deadline - Date.now(), 'deadline')
  if (!inventoryComplete || ownershipUncertain)
    stop('ownership-incomplete')
  const result = await Promise.race([completion, stopped])
  clearTimeout(timer)
  clearInterval(interval)
  // Command observation has ended, but mandatory owned-process cleanup can
  // still use the evidence reserve. Stop scheduling rolling work at this
  // boundary rather than waiting for the guard's eventual return.
  onCommandSettled?.()
  if (timedOut || interrupted || owned().some(row => row.pid !== child.pid && row.state !== 'Z')) {
    if (!timedOut && !interrupted) {
      interrupted = true
      persist('child-exited-with-live-descendants')
      signalOwned('SIGTERM')
    }

    // The direct child can exit while a descendant ignores TERM. Finish the
    // bounded cleanup rather than returning a false success with a live child.
    await new Promise(resolve => setTimeout(resolve, graceMs))
    leaderExpected = false
    signalOwned('SIGKILL')
    await new Promise(resolve => setTimeout(resolve, 25))
  }

  leaderExpected = false
  if (!child.stdio[4].destroyed && child.exitCode === null && child.signalCode === null)
    child.stdio[4].end(`${JSON.stringify({ type: 'release', code: result.code ?? 1 })}\n`)
  let closeTimer
  const didClose = await Promise.race([closed.then(() => true), new Promise(resolve => closeTimer = setTimeout(resolve, closeGraceMs, false))])
  clearTimeout(closeTimer)
  cleanupUnknown = !didClose || !inventoryComplete || ownershipUncertain || !containmentReady || owned().some(row => row.state !== 'Z')
  // SIGKILL is only a request for a D-state process. Persist UNKNOWN and detach
  // bounded IPC/pipe handles so the reserved artifact step can still run.
  if (cleanupUnknown) {
    child.stdout.destroy()
    child.stderr.destroy()
    child.stdio[3].destroy()
    child.stdio[4].destroy()
    child.unref()
  }

  process.off('SIGTERM', onTerm)
  process.off('SIGINT', onTerm)
  signal?.removeEventListener('abort', onTerm)
  persist('finished')
  if (spawnError)
    throw new Error('Diagnostic command could not start', { cause: spawnError })
  return timedOut ? 124 : interrupted ? 130 : cleanupUnknown ? 125 : result.code ?? 1
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [output, minutes, separator, command, ...args] = process.argv.slice(2)
  if (!output || separator !== '--' || !command)
    throw new Error('Usage: ci-deadline.mjs OUTPUT JOB_MINUTES -- COMMAND [ARGUMENTS]')
  process.exitCode = await observeCommand({
    output,
    jobMinutes: Number(minutes),
    jobStartedAt: Number(process.env.CI_JOB_STARTED_AT) * 1000,
    command,
    args,
  })
}
