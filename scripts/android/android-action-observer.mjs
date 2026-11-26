import { spawn } from 'node:child_process'
import {
  closeSync,
  fsyncSync,
  linkSync,
  mkdirSync,
  opendirSync,
  openSync,
  readFileSync,
  readlinkSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import { performance } from 'node:perf_hooks'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

export const SAMPLE_OFFSETS_MS = Object.freeze([0, 1000, 5000, 15000, 30000, 60000, 120000])
export const OBSERVER_HARD_STOP_MS = 120000
export const MAX_RECORDED_PROCESS_COUNT = 128

const MAX_PROC_ENTRIES = 1024
const MAX_FD_ENTRIES = 512
const STDIO_FDS = ['0', '1', '2']
const NODE24_MAIN_THREAD_COMM = 'MainThread'
const scriptPath = fileURLToPath(import.meta.url)

function errorKind(error) {
  if (error?.code === 'EACCES' || error?.code === 'EPERM')
    return 'permission-denied'
  if (error?.code === 'ENOENT' || error?.code === 'ESRCH')
    return 'process-disappeared'
  return 'unavailable'
}

export function parseProcStat(value) {
  const open = value.indexOf('(')
  const close = value.lastIndexOf(')')
  if (open <= 0 || close <= open)
    return null

  const pid = Number(value.slice(0, open).trim())
  const comm = value.slice(open + 1, close)
  const fields = value.slice(close + 1).trim().split(/\s+/)
  const ppid = Number(fields[1])
  const startTicks = fields[19]
  if (!Number.isSafeInteger(pid) || pid < 1 || !Number.isSafeInteger(ppid) || ppid < 0 || !/^\d+$/.test(startTicks ?? '')) {
    return null
  }

  return {
    pid,
    ppid,
    startTicks,
    state: fields[0],
    comm,
  }
}

function readStat(procRoot, pid) {
  return parseProcStat(readFileSync(path.join(procRoot, String(pid), 'stat'), 'utf8'))
}

function isNode24MainThread(procRoot, stat) {
  if (stat.comm !== NODE24_MAIN_THREAD_COMM)
    return false

  try {
    const executable = readlinkSync(path.join(procRoot, String(stat.pid), 'exe'))
    if (path.basename(executable) !== 'node')
      return false

    const current = readStat(procRoot, stat.pid)
    return current?.comm === NODE24_MAIN_THREAD_COMM && current.startTicks === stat.startTicks
  }
  catch {
    return false
  }
}

function isActionNode(procRoot, stat) {
  return stat.comm === 'node' || isNode24MainThread(procRoot, stat)
}

function isWhitelisted(procRoot, stat) {
  const { comm } = stat
  if (!/^[\w.-]{1,15}$/.test(comm))
    return false
  return isActionNode(procRoot, stat)
    || ['sh', 'bash', 'dash', 'adb', 'emulator'].includes(comm)
    || comm.startsWith('emulator-')
    || comm.startsWith('emulator_')
    || comm.startsWith('qemu-system-')
}

export function classifyFdTarget(target) {
  if (target.startsWith('socket:['))
    return 'socket'
  if (target.startsWith('pipe:['))
    return 'pipe'
  if (target.startsWith('anon_inode:['))
    return 'anon_inode'
  if (target === '/dev/null' || target === '/dev/null (deleted)')
    return 'devnull'
  return 'other'
}

function numericEntries(directoryPath, limit) {
  let directory
  try {
    directory = opendirSync(directoryPath)
  }
  catch (error) {
    return { entries: [], status: errorKind(error) }
  }

  const entries = []
  let scanned = 0
  let status = 'complete'
  try {
    while (scanned <= limit) {
      const entry = directory.readSync()
      if (!entry)
        break
      scanned += 1
      if (scanned > limit) {
        status = 'limit-reached'
        break
      }

      if (/^\d+$/.test(entry.name))
        entries.push(entry.name)
    }
  }
  catch (error) {
    status = errorKind(error)
  }
  finally {
    try {
      directory.closeSync()
    }
    catch {
      if (status === 'complete')
        status = 'unavailable'
    }
  }

  return { entries, status }
}

function descriptorSnapshot(procRoot, stat) {
  const counts = {
    socket: 0,
    pipe: 0,
    anon_inode: 0,
    devnull: 0,
    other: 0,
  }
  const stdio = { 0: 'unavailable', 1: 'unavailable', 2: 'unavailable' }
  const directoryPath = path.join(procRoot, String(stat.pid), 'fd')
  const listing = numericEntries(directoryPath, MAX_FD_ENTRIES)
  if (listing.status === 'permission-denied' || listing.status === 'process-disappeared' || listing.status === 'unavailable') {
    return { counts, stdio, status: listing.status }
  }

  const fds = listing.entries.sort((left, right) => {
    const leftIndex = STDIO_FDS.indexOf(left)
    const rightIndex = STDIO_FDS.indexOf(right)
    if (leftIndex !== -1 || rightIndex !== -1) {
      if (leftIndex === -1)
        return 1
      if (rightIndex === -1)
        return -1
      return leftIndex - rightIndex
    }

    return Number(left) - Number(right)
  })

  let status = listing.status
  for (const fd of fds) {
    try {
      const kind = classifyFdTarget(readlinkSync(path.join(directoryPath, fd)))
      counts[kind] += 1
      if (STDIO_FDS.includes(fd))
        stdio[fd] = kind
    }
    catch (error) {
      if (error?.code === 'EACCES' || error?.code === 'EPERM')
        status = 'permission-denied'
      else if (error?.code === 'ESRCH')
        status = 'process-disappeared'
    }
  }

  try {
    const current = readStat(procRoot, stat.pid)
    if (!current || current.startTicks !== stat.startTicks) {
      return {
        counts: {
          socket: 0,
          pipe: 0,
          anon_inode: 0,
          devnull: 0,
          other: 0,
        },
        stdio: { 0: 'unavailable', 1: 'unavailable', 2: 'unavailable' },
        status: 'process-disappeared',
      }
    }
  }
  catch (error) {
    return {
      counts: {
        socket: 0,
        pipe: 0,
        anon_inode: 0,
        devnull: 0,
        other: 0,
      },
      stdio: { 0: 'unavailable', 1: 'unavailable', 2: 'unavailable' },
      status: errorKind(error),
    }
  }

  return { counts, stdio, status }
}

function processRecord(procRoot, stat) {
  const fds = descriptorSnapshot(procRoot, stat)
  return {
    pid: stat.pid,
    ppid: stat.ppid,
    startTicks: stat.startTicks,
    state: stat.state,
    comm: stat.comm,
    fdTypes: fds.counts,
    stdio: fds.stdio,
    fdScan: fds.status,
  }
}

export function collectSnapshot({
  procRoot = '/proc',
  actionNode,
  sampleIndex,
  scheduledOffsetMs,
  elapsedMs,
  excludedPids = [],
}) {
  let actionStat
  let actionStatus
  try {
    actionStat = readStat(procRoot, actionNode.pid)
    actionStatus = !actionStat ? 'unavailable' : actionStat.startTicks === actionNode.startTicks ? 'present' : 'reused'
  }
  catch (error) {
    actionStatus = errorKind(error) === 'process-disappeared' ? 'absent' : errorKind(error)
  }

  const listing = numericEntries(procRoot, MAX_PROC_ENTRIES)
  const pids = listing.entries.sort((left, right) => Number(left) - Number(right))
  const excluded = new Set([...excludedPids, process.pid])
  const records = []
  const seen = new Set()
  let processScan = listing.status

  const append = (stat, actionProcess = false) => {
    if (!stat || seen.has(stat.pid) || stat.pid === process.pid || (!actionProcess && excluded.has(stat.pid)) || !isWhitelisted(procRoot, stat))
      return
    if (records.length >= MAX_RECORDED_PROCESS_COUNT) {
      processScan = 'limit-reached'
      return
    }

    seen.add(stat.pid)
    records.push(processRecord(procRoot, stat))
  }

  if (actionStat?.startTicks === actionNode.startTicks)
    append(actionStat, true)
  for (const pid of pids) {
    if (seen.has(Number(pid)) || excluded.has(Number(pid)))
      continue
    if (records.length >= MAX_RECORDED_PROCESS_COUNT) {
      processScan = 'limit-reached'
      break
    }

    try {
      append(readStat(procRoot, pid))
    }
    catch (error) {
      if (error?.code === 'EACCES' || error?.code === 'EPERM')
        processScan = 'permission-denied'
    }
  }

  return {
    schema: 1,
    sampleIndex,
    scheduledOffsetMs,
    elapsedMs,
    actionNode: { pid: actionNode.pid, startTicks: actionNode.startTicks, status: actionStatus },
    processScan,
    processes: records,
  }
}

export function findActionNodeAncestor(procRoot = '/proc') {
  let current
  try {
    current = readStat(procRoot, process.pid)
  }
  catch {
    return null
  }

  const visited = new Set([process.pid])
  let pid = current?.ppid
  for (let depth = 0; depth < 32 && pid > 1; depth += 1) {
    if (visited.has(pid))
      return null
    visited.add(pid)
    let parent
    try {
      parent = readStat(procRoot, pid)
    }
    catch {
      return null
    }

    if (!parent)
      return null
    if (isActionNode(procRoot, parent))
      return { pid: parent.pid, startTicks: parent.startTicks }
    pid = parent.ppid
  }

  return null
}

export function startObservation({
  outputDirectory = path.join('test-results', 'android-direct', 'closure-observation'),
  actionNode = findActionNodeAncestor(),
} = {}) {
  if (!actionNode) {
    writeObserverStatus(outputDirectory, 'ancestor-not-found')
    return null
  }

  if (!Number.isSafeInteger(actionNode.pid) || actionNode.pid < 1 || !/^\d+$/.test(actionNode.startTicks)) {
    writeObserverStatus(outputDirectory, 'invalid-target')
    return null
  }

  writeObserverStatus(outputDirectory, 'sampler-starting')
  let observer
  try {
    observer = spawn(process.execPath, [
      scriptPath,
      'observe',
      String(actionNode.pid),
      actionNode.startTicks,
      path.resolve(outputDirectory),
      String(process.pid),
    ], { detached: true, stdio: 'ignore', env: {} })
  }
  catch {
    writeObserverStatus(outputDirectory, 'sampler-start-failed')
    return null
  }

  observer.once('error', () => writeObserverStatus(outputDirectory, 'sampler-start-failed'))
  observer.unref()
  return observer
}

function writeObserverStatus(directory, status, sampleIndex) {
  const receipt = { schema: 1, status }
  if (Number.isSafeInteger(sampleIndex) && sampleIndex >= 0 && sampleIndex < SAMPLE_OFFSETS_MS.length)
    receipt.sampleIndex = sampleIndex

  let descriptor
  let pending
  try {
    mkdirSync(directory, { recursive: true, mode: 0o700 })
    const filename = path.join(directory, 'observer-status.json')
    pending = path.join(directory, `.observer-status.pending-${process.pid}-${status}-${sampleIndex ?? 'startup'}`)
    descriptor = openSync(pending, 'wx', 0o600)
    writeFileSync(descriptor, `${JSON.stringify(receipt)}\n`)
    fsyncSync(descriptor)
    closeSync(descriptor)
    descriptor = undefined
    renameSync(pending, filename)
    pending = undefined
  }
  catch {
    // Observer receipts are best-effort and must not affect the Android scenario.
  }
  finally {
    if (descriptor !== undefined) {
      try {
        closeSync(descriptor)
      }
      catch {}
    }

    if (pending) {
      try {
        unlinkSync(pending)
      }
      catch {}
    }
  }
}

function writeSnapshot(directory, index, snapshot) {
  mkdirSync(directory, { recursive: true, mode: 0o700 })
  const filename = `sample-${String(index).padStart(3, '0')}.json`
  const pending = path.join(directory, `.${filename}.pending-${process.pid}`)
  const descriptor = openSync(pending, 'wx', 0o600)
  try {
    writeFileSync(descriptor, `${JSON.stringify(snapshot)}\n`)
    fsyncSync(descriptor)
  }
  finally {
    closeSync(descriptor)
  }

  try {
    linkSync(pending, path.join(directory, filename))
  }
  finally {
    unlinkSync(pending)
  }
}

function waitUntil(target) {
  const remaining = target - performance.now()
  if (remaining <= 0)
    return Promise.resolve()
  return new Promise(resolve => setTimeout(resolve, remaining))
}

async function observe(actionNode, outputDirectory, excludedPids) {
  const startedAt = performance.now()
  for (let index = 0; index < SAMPLE_OFFSETS_MS.length; index += 1) {
    const offset = SAMPLE_OFFSETS_MS[index]
    await waitUntil(startedAt + offset)
    const elapsedMs = Math.min(OBSERVER_HARD_STOP_MS, Math.max(0, Math.round(performance.now() - startedAt)))
    const snapshot = collectSnapshot({
      actionNode,
      sampleIndex: index,
      scheduledOffsetMs: offset,
      elapsedMs,
      excludedPids,
    })
    try {
      writeSnapshot(outputDirectory, index, snapshot)
    }
    catch {
      writeObserverStatus(outputDirectory, 'sample-write-failed', index)
      return
    }

    if (index === 0)
      writeObserverStatus(outputDirectory, 'first-sample-written', index)

    if (snapshot.actionNode.status === 'absent' || snapshot.actionNode.status === 'reused')
      return
  }
}

async function runCli() {
  const [mode, pidArgument, startTicks, outputDirectory, excludedPidArgument] = process.argv.slice(2)
  if (mode === 'start') {
    try {
      startObservation({ outputDirectory: pidArgument })
    }
    catch {
      // Observation setup must not alter the test result.
    }

    return
  }

  if (mode !== 'observe' || !/^\d+$/.test(pidArgument ?? '') || !/^\d+$/.test(startTicks ?? '') || !outputDirectory)
    return
  const pid = Number(pidArgument)
  const excludedPid = Number(excludedPidArgument)
  if (!Number.isSafeInteger(pid) || pid < 1 || !Number.isSafeInteger(excludedPid) || excludedPid < 1)
    return

  writeObserverStatus(outputDirectory, 'sampler-started')
  await observe({ pid, startTicks }, outputDirectory, [excludedPid])
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  runCli().catch(() => {})
}
