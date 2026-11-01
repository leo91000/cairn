import { spawn, spawnSync } from 'node:child_process'
import { constants } from 'node:os'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const aptOptions = ['-o', 'Acquire::http::Timeout=20', '-o', 'Acquire::https::Timeout=20', '-o', 'DPkg::Lock::Timeout=20']
const phases = {
  update: { args: [...aptOptions, '-o', 'APT::Update::Error-Mode=any', 'update', '-qq'] },
  install: { args: [...aptOptions, 'install', '-y', '--no-install-recommends'] },
}
const profiles = {
  'native': { seconds: { update: 120, install: 360 }, packages: ['libclang-dev', 'age', 'e2fsprogs', 'fakeroot'] },
  'android-network': { seconds: { update: 90, install: 90 }, packages: ['iproute2', 'iptables', 'util-linux'] },
}

// A full or closed diagnostic pipe must not hold supervision open. printf has no
// descendants, and its bounded write cannot leave pending I/O in the owner.
function writeMarker(line) {
  return spawnSync('/usr/bin/printf', ['%s\n', line], {
    stdio: ['ignore', 'inherit', 'ignore'],
    timeout: 100,
    killSignal: 'SIGKILL',
  })
}

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

export async function runAptPhase(phase, seconds, cleanupGraceMs = 1000, profile = 'native') {
  if (!Object.hasOwn(phases, phase) || !Object.hasOwn(profiles, profile))
    return 2

  const specification = phases[phase]
  const profileSpecification = profiles[profile]
  const maximumSeconds = profileSpecification.seconds[phase]
  const limitSeconds = seconds ?? maximumSeconds
  if (!Number.isFinite(limitSeconds) || limitSeconds <= 0 || limitSeconds > maximumSeconds
    || !Number.isFinite(cleanupGraceMs) || cleanupGraceMs <= 0 || cleanupGraceMs > 1000) {
    return 2
  }

  marker(writeMarker, phase, 'apt-start', `profile=${profile}`)
  const code = await new Promise((resolve) => {
    let child
    let stopping = false
    let finished = false
    let cleanupTimer
    let workTimer
    const terminate = () => cleanup(128 + constants.signals.SIGTERM)
    const interrupt = () => cleanup(128 + constants.signals.SIGINT)

    function killGroup(signal) {
      if (!child?.pid)
        return false
      try {
        process.kill(-child.pid, signal)
        return true
      }
      catch (error) {
        if (error.code === 'ESRCH')
          return false
        marker(writeMarker, phase, 'cleanup-error', `signal=${signal}`)
        return true
      }
    }

    function finish(status) {
      if (finished)
        return
      finished = true
      clearTimeout(cleanupTimer)
      clearTimeout(workTimer)
      process.removeListener('SIGTERM', terminate)
      process.removeListener('SIGINT', interrupt)
      resolve(status)
    }

    function cleanup(status) {
      if (stopping)
        return
      stopping = true
      clearTimeout(workTimer)
      if (!killGroup('SIGTERM')) {
        finish(status)
        return
      }

      // A leader exiting does not prove its descendants stopped. Keep supervision
      // alive until the owned group receives its hard kill, even after leader exit.
      cleanupTimer = setTimeout(() => {
        killGroup('SIGKILL')
        finish(status)
      }, cleanupGraceMs)
    }

    process.on('SIGTERM', terminate)
    process.on('SIGINT', interrupt)
    const args = phase === 'install' ? [...specification.args, ...profileSpecification.packages] : specification.args
    child = spawn('apt-get', args, { detached: true, stdio: ['ignore', 'inherit', 'inherit'] })
    workTimer = setTimeout(cleanup, limitSeconds * 1000, 124)
    child.on('error', () => {
      if (!stopping)
        cleanup(1)
    })
    child.on('exit', (status, signal) => {
      if (!stopping)
        cleanup(exitCode({ status, signal }))
    })
  })
  marker(writeMarker, phase, 'apt-end', `code=${code} profile=${profile}`)
  return code
}

export function installNativeCiDependencies({
  profile = 'native',
  phaseSeconds = {},
  entryAllowanceSeconds = 30,
  killAfterSeconds = 5,
  write = writeMarker,
} = {}) {
  if (!Object.hasOwn(profiles, profile))
    return 2

  for (const phase of Object.keys(phases)) {
    const seconds = phaseSeconds[phase] ?? profiles[profile].seconds[phase]
    const started = Date.now()
    marker(write, phase, 'start', `limit_seconds=${seconds} profile=${profile}`)

    // The privileged owner bounds APT work and cleans its process group. These
    // fail-safe deadlines cover privilege entry and stalled supervision too.
    const result = spawnSync('timeout', [
      `--kill-after=${killAfterSeconds}s`,
      `${seconds + entryAllowanceSeconds + killAfterSeconds}s`,
      'sudo',
      '-n',
      'env',
      'DEBIAN_FRONTEND=noninteractive',
      'timeout',
      `--kill-after=${killAfterSeconds}s`,
      `${seconds + killAfterSeconds}s`,
      process.execPath,
      fileURLToPath(import.meta.url),
      '--apt-phase',
      phase,
      `${seconds}`,
      `${Math.min(1000, killAfterSeconds * 500)}`,
      profile,
    ], { stdio: ['ignore', 'inherit', 'inherit'] })
    const code = exitCode(result)
    marker(write, phase, 'end', `code=${code} elapsed_ms=${Date.now() - started} profile=${profile}`)
    if (code !== 0)
      return code
  }

  return 0
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  if (args.length === 0)
    process.exitCode = installNativeCiDependencies()
  else if (args.length === 2 && args[0] === '--profile')
    process.exitCode = installNativeCiDependencies({ profile: args[1] })
  else if (args.length === 5 && args[0] === '--apt-phase')
    process.exitCode = await runAptPhase(args[1], Number(args[2]), Number(args[3]), args[4])
  else
    process.exitCode = 2
}
