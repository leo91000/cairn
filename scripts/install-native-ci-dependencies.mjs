import { spawn, spawnSync } from 'node:child_process'
import { constants } from 'node:os'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const aptOptions = ['-o', 'Acquire::http::Timeout=20', '-o', 'Acquire::https::Timeout=20', '-o', 'DPkg::Lock::Timeout=20']
const phases = {
  update: { seconds: 120, args: [...aptOptions, '-o', 'APT::Update::Error-Mode=any', 'update', '-qq'] },
  install: { seconds: 360, args: [...aptOptions, 'install', '-y', '--no-install-recommends', 'libclang-dev', 'age', 'e2fsprogs', 'fakeroot'] },
}

// Diagnostics must never abort supervision or replace the package-manager status.
process.stdout.on('error', () => {})
const writeMarker = line => process.stdout.write(`${line}\n`, () => {})

function marker(write, phase, event, detail = '') {
  try {
    write(`[native-ci-dependencies] phase=${phase} event=${event}${detail ? ` ${detail}` : ''}`)
  }
  catch {
    // Closed or unavailable diagnostic output does not change setup behavior.
  }
}

function exitCode(result) {
  if (result.status !== null)
    return result.status
  return result.signal ? 128 + (constants.signals[result.signal] ?? 0) : 1
}

export async function runAptPhase(phase, cleanupGraceMs = 1000) {
  if (!Object.hasOwn(phases, phase))
    return 2

  const specification = phases[phase]
  marker(writeMarker, phase, 'apt-start')
  const code = await new Promise((resolve) => {
    let child
    let stopping = false
    let finished = false
    let cleanupTimer
    const terminate = () => stop('SIGTERM')
    const interrupt = () => stop('SIGINT')

    function killGroup(signal) {
      if (!child?.pid)
        return
      try {
        process.kill(-child.pid, signal)
      }
      catch {
        // The group may already have exited; still attempt the later hard kill.
      }
    }

    function finish(status) {
      if (finished)
        return
      finished = true
      clearTimeout(cleanupTimer)
      process.removeListener('SIGTERM', terminate)
      process.removeListener('SIGINT', interrupt)
      resolve(status)
    }

    function stop(signal) {
      if (stopping)
        return
      stopping = true
      killGroup('SIGTERM')
      // A leader exiting does not prove its descendants stopped. Keep supervision
      // alive until the owned group receives its hard kill, even after leader exit.
      cleanupTimer = setTimeout(() => {
        killGroup('SIGKILL')
        finish(128 + constants.signals[signal])
      }, cleanupGraceMs)
    }

    process.on('SIGTERM', terminate)
    process.on('SIGINT', interrupt)
    child = spawn('apt-get', specification.args, { detached: true, stdio: ['ignore', 'inherit', 'inherit'] })
    child.on('error', () => {
      if (!stopping)
        finish(1)
    })
    child.on('exit', (status, signal) => {
      if (!stopping)
        finish(exitCode({ status, signal }))
    })
  })
  marker(writeMarker, phase, 'apt-end', `code=${code}`)
  return code
}

export function installNativeCiDependencies({
  phaseSeconds = {},
  entryAllowanceSeconds = 30,
  killAfterSeconds = 5,
  write = writeMarker,
} = {}) {
  for (const [phase, specification] of Object.entries(phases)) {
    const seconds = phaseSeconds[phase] ?? specification.seconds
    const started = Date.now()
    marker(write, phase, 'start', `limit_seconds=${seconds}`)

    // The outer deadline also covers sudo entry; the privileged inner deadline
    // can terminate root-owned APT/dpkg descendants as a process group.
    const result = spawnSync('timeout', [
      `--kill-after=${killAfterSeconds}s`,
      `${seconds + entryAllowanceSeconds}s`,
      'sudo',
      '-n',
      'env',
      'DEBIAN_FRONTEND=noninteractive',
      'timeout',
      `--kill-after=${killAfterSeconds}s`,
      `${seconds}s`,
      process.execPath,
      fileURLToPath(import.meta.url),
      '--apt-phase',
      phase,
      `${Math.min(1000, killAfterSeconds * 500)}`,
    ], { stdio: ['ignore', 'inherit', 'inherit'] })
    const code = exitCode(result)
    marker(write, phase, 'end', `code=${code} elapsed_ms=${Date.now() - started}`)
    if (code !== 0)
      return code
  }

  return 0
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  const cleanupGraceMs = Number(args[2])
  process.exitCode = args.length === 0
    ? installNativeCiDependencies()
    : args.length === 3 && args[0] === '--apt-phase' && Number.isFinite(cleanupGraceMs) && cleanupGraceMs > 0 && cleanupGraceMs <= 1000
      ? await runAptPhase(args[1], cleanupGraceMs)
      : 2
}
