import { spawn, spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync } from 'node:fs'
import { createConnection, createServer } from 'node:net'
import { constants, tmpdir } from 'node:os'
import { join } from 'node:path'
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

export async function runAptPhase(phase, seconds, cleanupGraceMs = 1000, profile = 'native', controlPath) {
  if (!Object.hasOwn(phases, phase) || !Object.hasOwn(profiles, profile) || typeof controlPath !== 'string' || !controlPath)
    return 2

  const specification = phases[phase]
  const profileSpecification = profiles[profile]
  const maximumSeconds = profileSpecification.seconds[phase]
  const limitSeconds = seconds ?? maximumSeconds
  if (!Number.isFinite(limitSeconds) || limitSeconds <= 0 || limitSeconds > maximumSeconds
    || !Number.isFinite(cleanupGraceMs) || cleanupGraceMs <= 0 || cleanupGraceMs > 1000) {
    return 2
  }

  marker(writeMarker, phase, 'entry-ready', `profile=${profile}`)
  const code = await new Promise((resolve) => {
    let child
    let stopping = false
    let finished = false
    let cleanupTimer
    let workTimer
    let controlSocket
    let controlInput = ''
    const terminate = () => cleanup(128 + constants.signals.SIGTERM)
    const interrupt = () => cleanup(128 + constants.signals.SIGINT)
    const control = (bytes) => {
      controlInput += bytes
      if (controlInput.length > 32) {
        cleanup(2)
        return
      }

      let authorized = false
      while (controlInput.includes('\n')) {
        const boundary = controlInput.indexOf('\n')
        const command = controlInput.slice(0, boundary)
        controlInput = controlInput.slice(boundary + 1)
        if (command === 'SIGINT')
          interrupt()
        else if (command === 'SIGTERM')
          terminate()
        else if (command === 'START')
          authorized = true
        else
          cleanup(2)
      }

      if (authorized && !stopping && !child)
        startApt()
    }

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
      controlSocket?.destroy()
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

    function startApt() {
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
      marker(writeMarker, phase, 'apt-start', `profile=${profile}`)
    }

    // Connect only after privilege entry. The live launcher then authorizes
    // package work, or refuses it if cancellation happened while entry waited.
    controlSocket = createConnection(controlPath)
    controlSocket.on('data', control)
    controlSocket.on('end', terminate)
    controlSocket.on('error', () => cleanup(1))
  })
  marker(writeMarker, phase, 'apt-end', `code=${code} profile=${profile}`)
  return code
}

export async function installNativeCiDependencies({
  profile = 'native',
  phaseSeconds = {},
  entryAllowanceSeconds = 30,
  killAfterSeconds = 5,
  write = writeMarker,
} = {}) {
  if (!Object.hasOwn(profiles, profile))
    return 2

  let interruptedCode
  let child
  let controlSocket
  let controlDirectory
  let controlServer
  let interruptionTimer
  const interrupt = (signal) => {
    if (interruptedCode)
      return
    interruptedCode = 128 + constants.signals[signal]
    controlSocket?.end(`${signal}\n`)
    // Privilege entry might stall before it can consume the control request.
    // Keep the existing timeout supervisor as a bounded fallback in that case.
    interruptionTimer = setTimeout(() => {
      try {
        process.kill(-child.pid, 'SIGTERM')
      }
      catch {
        // The owned supervisor already exited.
      }
    }, killAfterSeconds * 1000)
  }

  const terminate = () => interrupt('SIGTERM')
  const keyboardInterrupt = () => interrupt('SIGINT')
  process.on('SIGTERM', terminate)
  process.on('SIGINT', keyboardInterrupt)

  try {
    controlDirectory = mkdtempSync(join(tmpdir(), 'cairn-apt-control-'))
    const controlPath = join(controlDirectory, 'owner.sock')
    controlServer = createServer((socket) => {
      if (controlSocket) {
        socket.destroy()
        return
      }

      controlSocket = socket
      socket.on('error', () => {})
      if (interruptedCode)
        socket.end(`${interruptedCode === 130 ? 'SIGINT' : 'SIGTERM'}\n`)
      else
        socket.write('START\n')
    })
    await new Promise((resolve, reject) => {
      controlServer.once('error', reject)
      controlServer.listen(controlPath, resolve)
    })
    for (const phase of Object.keys(phases)) {
      if (interruptedCode)
        return interruptedCode
      const seconds = phaseSeconds[phase] ?? profiles[profile].seconds[phase]
      const started = Date.now()
      marker(write, phase, 'start', `limit_seconds=${seconds} profile=${profile}`)

      // The privileged owner bounds APT work and cleans its process group. These
      // fail-safe deadlines cover privilege entry and stalled supervision too.
      const args = [
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
        controlPath,
      ]
      const result = await new Promise((resolve) => {
        child = spawn('timeout', args, { detached: true, stdio: ['ignore', 'inherit', 'inherit'] })
        child.on('error', () => resolve({ status: 1, signal: null }))
        child.on('close', (status, signal) => resolve({ status, signal }))
      })
      clearTimeout(interruptionTimer)
      controlSocket?.destroy()
      controlSocket = undefined
      const code = interruptedCode ?? exitCode(result)
      marker(write, phase, 'end', `code=${code} elapsed_ms=${Date.now() - started} profile=${profile}`)
      if (code !== 0)
        return code
    }

    return 0
  }
  catch {
    return interruptedCode ?? 1
  }
  finally {
    clearTimeout(interruptionTimer)
    process.removeListener('SIGTERM', terminate)
    process.removeListener('SIGINT', keyboardInterrupt)
    controlSocket?.destroy()
    if (controlServer)
      await new Promise(resolve => controlServer.close(resolve))
    if (controlDirectory)
      rmSync(controlDirectory, { recursive: true, force: true })
  }
}

if (import.meta.main) {
  const args = process.argv.slice(2)
  if (args.length === 0)
    process.exitCode = await installNativeCiDependencies()
  else if (args.length === 2 && args[0] === '--profile')
    process.exitCode = await installNativeCiDependencies({ profile: args[1] })
  else if (args.length === 6 && args[0] === '--apt-phase')
    process.exitCode = await runAptPhase(args[1], Number(args[2]), Number(args[3]), args[4], args[5])
  else
    process.exitCode = 2
}
