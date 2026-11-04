import { Buffer } from 'node:buffer'
import { execFileSync, spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  closeSync,
  constants,
  fstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { dependencyOptions, verifyBackendDependencies } from './ci-backend-dependencies.mjs'
import { createPrivateTrace, exitCode, writeLifecycle } from './ci-backend-trace.mjs'
import { observeCommand } from './ci-deadline.mjs'

export const UPLOADER_SHA = '043fb46d1a93c77aae656e7c1c64a875d1fc6a0a' // upload-artifact v7.0.1
export const BACKEND_COMMAND = 'node' // Preserve setup-pnpm's PATH-selected runtime.
export const BACKEND_ARGS = ['scripts/test-backend.mjs', '--nextest', '--test-threads', '8', '--no-fail-fast', '--color', 'never', '--status-level', 'all']
// Selection is available only inside main's validated diagnostic startup path.
export function backendDiagnosticCommand(selector = 'full', workspace) {
  if (selector === 'full')
    return { command: BACKEND_COMMAND, args: BACKEND_ARGS }
  if (selector === 'avatar-authority' && path.isAbsolute(workspace))
    return { command: BACKEND_COMMAND, args: [path.join(workspace, 'scripts/ci-avatar-authority-diagnostic.mjs')] }
  throw new Error('Unknown backend diagnostic selector')
}

export function avatarWorkerProof(output, candidate) {
  let value
  try {
    value = readProof(path.join(output, 'avatar-worker.json'), 4096)
  }
  catch (error) {
    if (error.code === 'ENOENT')
      return { state: 'not-built', qualificationClaim: false }
    throw error
  }

  const sha = value => /^[a-f0-9]{64}$/.test(value ?? '')
  if (value.diagnosticOnly !== true || value.qualificationClaim !== false || value.selector !== 'avatar-authority'
    || !/^[a-f0-9]{40}$/.test(candidate ?? '') || value.source?.commit !== candidate || !/^[a-f0-9]{40}$/.test(value.source?.tree ?? '')
    || value.worker?.path !== 'target/debug/cairn' || !sha(value.worker?.sha256)
    || !Number.isSafeInteger(value.worker?.bytes) || value.worker.bytes < 1 || value.worker.bytes > 512 * 1024 * 1024
    || !sha(value.launcherSHA256) || !sha(value.fixtureSHA256) || !sha(value.avatarTestSHA256) || !sha(value.cargoLockSHA256) || value.iteration !== 1) {
    throw new Error('Invalid avatar worker provenance')
  }

  return {
    state: 'built',
    qualificationClaim: false,
    source: { commit: value.source.commit, tree: value.source.tree },
    worker: { path: 'target/debug/cairn', bytes: value.worker.bytes, sha256: value.worker.sha256 },
    launcherSHA256: value.launcherSHA256,
    fixtureSHA256: value.fixtureSHA256,
    avatarTestSHA256: value.avatarTestSHA256,
    cargoLockSHA256: value.cargoLockSHA256,
    iteration: 1,
  }
}

export function avatarDiagnosticPayload(payload, output, candidate) {
  const avatarWorker = avatarWorkerProof(output, candidate)
  if (payload.acquisition === 'finished' && payload.guardReturn?.code === 0 && avatarWorker.state !== 'built')
    throw new Error('Successful avatar diagnostic requires worker provenance')
  return { ...payload, avatarWorker }
}

export const PUBLICATION_LIMIT = 7 // Plus before-build, post-trace, after-build: <=10/job.
const phases = ['worker-fixture-build', 'code-worker-build', 'backend-tests']
const reasons = ['admitted-before-command', 'started', 'periodic', 'deadline', 'external-termination', 'ownership-incomplete', 'finished', 'child-exited-with-live-descendants', 'no-time-before-evidence-reserve']
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex')
const number = value => Number.isFinite(value) && value >= 0 ? value : undefined
const boolean = value => typeof value === 'boolean' ? value : undefined

// The parent directory and final file are opened without following symlinks.
// No glob, arbitrary file, log, cmdline or environment enters a checkpoint.
function readBounded(file, limit) {
  const directory = openSync(path.dirname(file), constants.O_RDONLY | constants.O_DIRECTORY | constants.O_NOFOLLOW)
  let fd
  try {
    fd = openSync(`/proc/self/fd/${directory}/${path.basename(file)}`, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
    const stat = fstatSync(fd)
    if (!stat.isFile() || stat.size > limit)
      throw new Error('Invalid diagnostic proof file')
    const bytes = Buffer.alloc(limit + 1)
    const length = readSync(fd, bytes, 0, bytes.length, 0)
    if (length > limit)
      throw new Error('Diagnostic proof exceeds its bound')
    return bytes.subarray(0, length)
  }
  finally {
    if (fd !== undefined)
      closeSync(fd)
    closeSync(directory)
  }
}

export function readProof(file, limit = 262144) {
  return JSON.parse(readBounded(file, limit).toString())
}

function counters(value, keys) {
  return Object.fromEntries(keys.flatMap(key => number(value?.[key]) === undefined ? [] : [[key, value[key]]]))
}

function cgroupIO(value) {
  const totals = {}
  if (typeof value !== 'string')
    return totals
  for (const match of value.slice(0, 2048).matchAll(/\b(rbytes|wbytes|rios|wios|dbytes|dios)=(\d+)\b/g)) {
    const amount = Number(match[2])
    if (Number.isSafeInteger(amount))
      totals[match[1]] = (totals[match[1]] ?? 0) + amount
  }

  return totals
}

export function checkpoint(proof) {
  const flags = ['timedOut', 'interrupted', 'inventoryComplete', 'ownershipUncertain', 'cleanupUnknown', 'containmentReady']
  if (!Array.isArray(proof.samples) || proof.samples.length < 1 || proof.samples.length > 12
    || number(proof.jobStartedAt) === undefined || number(proof.deadline) === undefined
    || flags.some(key => typeof proof[key] !== 'boolean')) {
    throw new Error('Invalid bounded diagnostic samples')
  }

  return {
    jobStartedAt: number(proof.jobStartedAt),
    deadline: number(proof.deadline),
    ...Object.fromEntries(flags.map(key => [key, boolean(proof[key])])),
    samples: proof.samples.map(sample => ({
      elapsedMs: number(sample.elapsedMs),
      reason: reasons.includes(sample.reason) ? sample.reason : 'unknown',
      phase: phases.includes(sample.phase) ? sample.phase : 'unknown',
      processCount: number(sample.processCount),
      availableMemoryKiB: number(sample.availableMemoryKiB),
      backend: {
        logBytes: number(sample.backend?.logBytes),
        tailBytes: number(sample.backend?.tailBytes),
        statusesInTail: counters(sample.backend?.statusesInTail, ['PASS', 'FAIL', 'SLOW', 'TIMEOUT']),
      },
      processes: (Array.isArray(sample.processes) ? sample.processes : []).slice(0, 16).map(item => ({
        pid: number(item.pid),
        parent: number(item.parent),
        startTicks: /^\d{1,24}$/.test(item.startTicks ?? '') ? item.startTicks : undefined,
        state: ['R', 'S', 'D', 'Z', 'T', 't', 'I', 'X'].includes(item.state) ? item.state : 'unknown',
        role: ['cargo', 'rustc', 'rust-lld', 'ld', 'cc', 'cargo-nextest', 'node', 'python3', 'python', 'fixture-process'].includes(item.role) ? item.role : 'unknown',
        cpuTicks: number(item.cpuTicks),
        io: counters(item.io, ['rchar', 'wchar', 'syscr', 'syscw', 'read_bytes', 'write_bytes', 'cancelled_write_bytes']),
        cgroupCpu: counters(item.cgroup?.cpu, ['usage_usec', 'user_usec', 'system_usec', 'nr_periods', 'nr_throttled', 'throttled_usec']),
        cgroupIO: cgroupIO(item.cgroup?.io),
        cgroupMemory: counters(item.cgroup?.memory, ['low', 'high', 'max', 'oom', 'oom_kill', 'oom_group_kill']),
      })),
    })),
  }
}

export function testedEnvironment(env) {
  return Object.fromEntries(Object.entries(env).filter(([key]) => !key.startsWith('ACTIONS_') && !key.startsWith('INPUT_')
    && !['GITHUB_TOKEN', 'GH_TOKEN', 'GITHUB_OUTPUT', 'GITHUB_ENV', 'GITHUB_PATH', 'GITHUB_STATE', 'GITHUB_STEP_SUMMARY'].includes(key)))
}

function identity(pid) {
  try {
    const fd = openSync(`/proc/${pid}/stat`, 'r')
    try {
      const bytes = Buffer.alloc(4096)
      const text = bytes.subarray(0, readSync(fd, bytes, 0, bytes.length, 0)).toString()
      return text.slice(text.lastIndexOf(')') + 2).trim().split(/\s+/)[19]
    }
    finally { closeSync(fd) }
  }
  catch { return undefined }
}

function outputValue(text, key, pattern) {
  const lines = text.split('\n')
  for (let index = 0; index < lines.length; index++) {
    const match = new RegExp(`^${key}<<([\\w-]+)$`).exec(lines[index])
    if (match && lines[index + 2] === match[1] && pattern.test(lines[index + 1]))
      return lines[index + 1]
    if (lines[index].startsWith(`${key}=`) && pattern.test(lines[index].slice(key.length + 1)))
      return lines[index].slice(key.length + 1)
  }

  throw new Error('Artifact upload did not return a committed receipt')
}

export function artifactPublisher({
  uploader,
  directory,
  env,
  uploadMs = 20000,
  closeMs = 1000,
  signal,
}) {
  let attempts = 0
  return async (payload, { signal: publicationSignal = signal } = {}) => {
    const startedAt = Number(env.CI_JOB_STARTED_AT) * 1000
    const reserve = payload.acquisition === 'finished' ? 0 : 120000
    const remaining = startedAt + 30 * 60000 - reserve - Date.now() - closeMs
    if (!Number.isFinite(startedAt) || payload.clock?.startedAt !== startedAt || remaining <= 0
      || ++attempts > PUBLICATION_LIMIT || publicationSignal?.aborted) {
      throw new Error('Durable evidence publication stopped')
    }

    const name = `backend-diagnostic-${env.GITHUB_RUN_ATTEMPT}-${attempts}`
    const snapshot = path.join(directory, `${name}.json`)
    const output = path.join(directory, `${name}.output`)
    const serialized = `${JSON.stringify(payload)}\n`
    if (Buffer.byteLength(serialized) > 262144)
      throw new Error('Durable snapshot exceeds its bound')
    writeFileSync(snapshot, serialized, { mode: 0o600, flag: 'wx' })
    writeFileSync(output, '', { mode: 0o600, flag: 'wx' })
    const publisherEnv = {
      ...Object.fromEntries(Object.entries(env).filter(([key]) => ['PATH', 'HOME', 'TMPDIR', 'GITHUB_WORKSPACE', 'GITHUB_REPOSITORY', 'GITHUB_RUN_ID', 'GITHUB_RUN_ATTEMPT', 'GITHUB_SERVER_URL', 'ACTIONS_RUNTIME_TOKEN', 'ACTIONS_RUNTIME_URL', 'ACTIONS_RESULTS_URL'].includes(key))),
      'GITHUB_OUTPUT': output,
      'GITHUB_STATE': '',
      'INPUT_NAME': name,
      'INPUT_PATH': snapshot,
      'INPUT_IF-NO-FILES-FOUND': 'error',
      'INPUT_RETENTION-DAYS': '14',
      'INPUT_COMPRESSION-LEVEL': '0',
      'INPUT_OVERWRITE': 'false',
      'INPUT_INCLUDE-HIDDEN-FILES': 'false',
      'INPUT_ARCHIVE': 'true',
    }
    const child = spawn(process.execPath, [uploader], { env: publisherEnv, stdio: ['ignore', 'pipe', 'pipe'], detached: true })
    const start = identity(child.pid)
    let stopped = false
    let closed = false
    let received = 0
    let finish
    const done = new Promise(resolve => finish = resolve)
    const stop = () => {
      if (stopped)
        return
      stopped = true
      if (start && identity(child.pid) === start)
        child.kill('SIGKILL')
      setTimeout(() => {
        if (!closed) {
          child.stdout.destroy()
          child.stderr.destroy()
          child.unref()
          finish(false)
        }
      }, closeMs).unref()
    }

    // Drain and discard SDK diagnostics: even malformed URL/server errors must
    // not surface a runtime credential in logs or a retained plaintext file.
    for (const stream of [child.stdout, child.stderr]) {
      stream.on('data', (bytes) => {
        received += bytes.length
        if (received > 65536)
          stop()
      })
    }

    child.once('error', () => finish(false))
    child.once('close', (code) => {
      closed = true
      finish(code === 0 && !stopped)
    })
    const timer = setTimeout(stop, Math.min(uploadMs, remaining))
    publicationSignal?.addEventListener('abort', stop, { once: true })
    if (publicationSignal?.aborted)
      stop()
    const success = await done
    clearTimeout(timer)
    publicationSignal?.removeEventListener('abort', stop)
    if (!success || !closed)
      throw new Error('Durable evidence upload failed or cleanup is unknown')
    const text = readBounded(output, 4096).toString()

    return { id: outputValue(text, 'artifact-id', /^\d+$/), digest: outputValue(text, 'artifact-digest', /^[a-f0-9]{64}$/) }
  }
}

export async function runDurableBackend({
  output,
  command,
  args,
  env,
  receipt,
  publish,
  firstCheckpointMs = 30000,
  checkpointMs = 360000,
  observeOptions = {},
  signal,
  onGuardReturn,
}) {
  const controller = new AbortController()
  let interrupted = false
  const stop = () => {
    interrupted = true
    controller.abort()
  }

  process.on('SIGTERM', stop)
  process.on('SIGINT', stop)
  signal?.addEventListener('abort', stop, { once: true })
  let timer
  let periodic = Promise.resolve()
  let failed = false
  let count = 0
  const snapshot = (state) => {
    // The existing ring includes kernel stacks/cgroup text. Allow its bounded
    // input, then publish only the much smaller fixed numeric projection.
    const proof = checkpoint(readProof(path.join(output, 'latest.json'), 4 * 1024 * 1024))
    if (proof.jobStartedAt !== receipt.clock.startedAt || proof.deadline !== receipt.clock.startedAt + 28 * 60000)
      throw new Error('Diagnostic checkpoint clock mismatch')
    return {
      ...receipt,
      acquisition: state,
      capturedAt: Date.now(),
      proof,
    }
  }

  const schedule = delay => timer = setTimeout(() => {
    periodic = Promise.resolve().then(() => publish(snapshot('periodic'), { signal: controller.signal })).catch(() => {
      failed = true
      controller.abort()
    }).then(() => {
      if (++count < PUBLICATION_LIMIT - 2 && !controller.signal.aborted)
        schedule(checkpointMs)
    })
  }, delay)
  if (signal?.aborted)
    stop()
  try {
    const status = await observeCommand({
      ...observeOptions,
      output,
      command,
      args,
      jobStartedAt: receipt.clock.startedAt,
      jobMinutes: 30,
      reserveSeconds: 120,
      commandEnv: testedEnvironment(env),
      signal: controller.signal,
      beforeCommand: async () => {
        const initial = snapshot('admitted-before-command')
        await publish(initial, { signal: controller.signal })
        if (!initial.proof.inventoryComplete || initial.proof.ownershipUncertain)
          throw new Error('Initial diagnostic inventory is incomplete')
        if (!controller.signal.aborted)
          schedule(firstCheckpointMs)
      },
    })
    const guardReturn = {
      state: 'returned',
      code: status,
      at: Date.now(),
      scope: 'owned-command-guard',
    }
    onGuardReturn?.(guardReturn)
    clearTimeout(timer)
    await periodic
    clearTimeout(timer)
    if (failed)
      return interrupted ? 130 : 125
    await publish({ ...snapshot('finished'), guardReturn }, { signal: controller.signal })
    return interrupted ? 130 : status
  }
  catch {
    return interrupted ? 130 : 125
  }
  finally {
    clearTimeout(timer)
    process.off('SIGTERM', stop)
    process.off('SIGINT', stop)
    signal?.removeEventListener('abort', stop)
  }
}

export async function runBackendAction({ log, ...options }) {
  const deadline = options.receipt.clock.startedAt + 30 * 60000
  const traceFailure = new AbortController()
  const trace = createPrivateTrace(log, { deadline, onFailure: () => traceFailure.abort() })
  let guardReturn = { state: 'unavailable', scope: 'owned-command-guard' }
  let status = 125
  let finalPublicationReturned = false
  let publicationAttempts = 0
  let publicationReturns = 0
  let traceResult
  try {
    if (await trace.ready) {
      status = await runDurableBackend({
        ...options,
        signal: options.signal ? AbortSignal.any([options.signal, traceFailure.signal]) : traceFailure.signal,
        observeOptions: { ...options.observeOptions, stdout: trace.stream, stderr: trace.stream },
        onGuardReturn: value => guardReturn = value,
        publish: async (payload, publicationOptions) => {
          if (payload.acquisition === 'finished') {
            traceResult = await trace.finish()
            payload = { ...payload, trace: traceResult, foregroundExit: 'not-observed' }
          }

          publicationAttempts++
          const receipt = await options.publish(payload, publicationOptions)
          publicationReturns++
          if (payload.acquisition === 'finished')
            finalPublicationReturned = true
          return receipt
        },
      })
    }
  }
  finally {
    traceResult = await trace.finish()
  }

  const successKnown = status !== 0 || (guardReturn.state === 'returned' && guardReturn.code === 0 && finalPublicationReturned)
  const code = traceResult.complete && successKnown ? exitCode(status) : 125
  let ownedClosureVerified = false
  try {
    const proof = readProof(path.join(options.output, 'latest.json'), 4 * 1024 * 1024)
    ownedClosureVerified = proof.cleanupUnknown === false && proof.ownershipUncertain === false
      && proof.inventoryComplete === true && proof.containmentReady === true
      && proof.samples.at(-1)?.reason === 'finished' && proof.samples.at(-1)?.processCount === 0
  }
  catch {}

  const publisherClosureVerified = publicationAttempts === publicationReturns
  const decision = (localIOUnknown) => {
    if (localIOUnknown && ownedClosureVerified && publisherClosureVerified) {
      return {
        kind: 'signal',
        signal: 'SIGKILL',
        reason: 'local-io-closure-unknown',
        scope: 'before-foreground-exit',
        ownedClosureVerified,
        publisherClosureVerified,
      }
    }

    return { kind: 'code', code: localIOUnknown ? 125 : code, scope: 'before-foreground-exit' }
  }

  const lifecycle = {
    guardReturn,
    diagnosticReturn: { state: 'returned', code: status },
    trace: traceResult,
    finalPublicationReturned,
    ownedClosureVerified,
    publisherClosureVerified,
    publicOutput: { stdoutPendingBytes: process.stdout.writableLength, stderrPendingBytes: process.stderr.writableLength },
    exitDecision: decision(!traceResult.fileClosed || !traceResult.streamClosed),
    foregroundExit: 'not-observed',
  }
  const record = await writeLifecycle(path.join(options.output, 'action-lifecycle.json'), lifecycle, deadline)
  if (record !== 'written')
    return { ...lifecycle, lifecycleRecord: record, exitDecision: record === 'unknown' ? decision(true) : { kind: 'code', code: 125, scope: 'lifecycle-record-failed' } }
  return lifecycle
}

export async function main() {
  try {
    const env = process.env
    const workspace = env.GITHUB_WORKSPACE
    const source = env.CI_DIAGNOSTIC_SOURCE
    const output = path.join(workspace, 'backend-diagnostics')
    const pinned = readProof(path.join(output, 'source.json'), 65536)
    const immutable = value => /^[a-f0-9]{40}$/.test(value ?? '')
    if (!source || !immutable(env.GITHUB_SHA) || pinned.observer !== env.GITHUB_SHA || !immutable(pinned.revisions?.candidate)
      || pinned.diagnosticOnly !== true || pinned.qualificationClaim !== false
      || pinned.clock?.startedAt !== Number(env.CI_JOB_STARTED_AT) * 1000 || !Number.isSafeInteger(pinned.clock?.jobId)
      || !/^\d+$/.test(env.GITHUB_RUN_ID ?? '') || !/^\d+$/.test(env.GITHUB_RUN_ATTEMPT ?? '')
      || !/^[\w.-]+\/[\w.-]+$/.test(env.GITHUB_REPOSITORY ?? '') || !env.ACTIONS_RUNTIME_TOKEN || !env.ACTIONS_RESULTS_URL) {
      throw new Error('Invalid diagnostic startup provenance')
    }

    const uploaderRoot = path.join(workspace, '.ci-artifact-uploader')
    const gitHead = cwd => execFileSync('git', ['rev-parse', 'HEAD'], { cwd, encoding: 'utf8', timeout: 10000 }).trim()
    if (gitHead(source) !== pinned.revisions.candidate || gitHead(uploaderRoot) !== UPLOADER_SHA)
      throw new Error('Diagnostic source or uploader identity mismatch')
    const dependenciesFile = path.join(output, 'dependencies.json')
    verifyBackendDependencies(dependencyOptions(env), readProof(dependenciesFile, 65536))
    const receipt = {
      diagnosticOnly: true,
      qualificationClaim: false,
      observer: pinned.observer,
      candidate: pinned.revisions.candidate,
      clock: { jobId: pinned.clock.jobId, startedAt: pinned.clock.startedAt },
      run: env.GITHUB_RUN_ID,
      attempt: env.GITHUB_RUN_ATTEMPT,
      repository: env.GITHUB_REPOSITORY,
      uploader: UPLOADER_SHA,
      supervisorSHA256: sha256(readBounded(path.join(workspace, 'scripts/ci-deadline.mjs'), 65536)),
      publisherSHA256: sha256(readBounded(path.join(workspace, 'scripts/ci-backend-durable.mjs'), 65536)),
      traceObserverSHA256: sha256(readBounded(path.join(workspace, 'scripts/ci-backend-trace.mjs'), 65536)),
      dependenciesSHA256: sha256(readBounded(dependenciesFile, 65536)),
      sourceManifestSHA256: sha256(readBounded(path.join(output, 'source.json'), 65536)),
      launcherSHA256: sha256(readBounded(path.join(source, 'scripts/test-backend.mjs'), 65536)),
      acquisition: 'unknown',
    }
    const selector = env.BACKEND_DIAGNOSTIC_SELECTOR || 'full'
    const selected = backendDiagnosticCommand(selector, workspace)
    receipt.selector = selector
    if (selector === 'avatar-authority')
      receipt.avatarObserverSHA256 = sha256(readBounded(path.join(workspace, 'scripts/ci-avatar-authority-diagnostic.mjs'), 65536))
    const directory = mkdtempSync(path.join(env.RUNNER_TEMP, 'cairn-backend-publication-'))
    mkdirSync(output, { recursive: true, mode: 0o700 })
    const publishArtifact = artifactPublisher({ uploader: path.join(uploaderRoot, 'dist/upload/index.js'), directory, env })
    const publish = (payload, options) => {
      if (selector !== 'avatar-authority')
        return publishArtifact(payload, options)
      return publishArtifact(avatarDiagnosticPayload(payload, output, pinned.revisions.candidate), options)
    }

    process.chdir(source)
    const result = await runBackendAction({
      log: path.join(workspace, 'backend-tests.log'),
      output,
      command: selected.command,
      args: selected.args,
      env: selector === 'avatar-authority' ? { ...env, CAIRN_AVATAR_DIAGNOSTIC_CANDIDATE: pinned.revisions.candidate, CAIRN_AVATAR_DIAGNOSTIC_OUTPUT: output } : env,
      receipt,
      publish,
    })
    return result
  }
  catch {
    console.error('Backend diagnostic evidence acquisition failed; no qualification claimed.')
    return 125
  }
}
