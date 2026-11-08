import { spawn, spawnSync } from 'node:child_process'
import {
  closeSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
} from 'node:fs'
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

// The privileged shell owns APT's group until cleanup finishes. Its GNU
// deadline runs independently of the Node control loop, including after an APT
// leader exits while descendants retain its group. No plan values enter shell
// source: the closed APT arguments are passed as positional parameters.
const aptOwner = String.raw`
set +e
set +m
grace="$1"
controller_pid="$2"
controller_started="$3"
cancel_path="$4"
closed_path="$5"
shift 5
stopped=0
trap 'stopped=124' ALRM
trap 'stopped=130' INT
trap 'stopped=143' TERM

controller_is_live() {
  local controller_stat
  local -a controller_fields
  IFS= read -r controller_stat 2>/dev/null < "/proc/$controller_pid/stat" || return 1
  read -r -a controller_fields <<< "${'$'}{controller_stat##*) }"
  [ "${'$'}{controller_fields[19]}" = "$controller_started" ] &&
    [ "${'$'}{controller_fields[0]}" != Z ]
}

if [ "$stopped" -ne 0 ]; then
  exit "$stopped"
fi
# The session leader reports its group before waiting for admission. EOF refuses
# package work, including if the controller disappears before registering it.
setsid bash -c '
  cancel_path="$1"
  shift
  printf "%s\n" "$$" >&3 || exit 1
  IFS= read -r admission <&4 || exit 143
  [ "$admission" = START ] || exit 143
  [ ! -d "$cancel_path" ] || exit 143
  exec 3>&- 4<&-
  exec "$@"
' cairn-native-apt-worker "$cancel_path" "$@" &
apt_pid=$!
# The private cancellation marker is independent of Node's control socket and
# sudo's signal relay. Polling schedules observation; the GNU work bound remains
# authoritative even if this loop cannot progress.
while [ "$stopped" -eq 0 ] && kill -0 "$apt_pid" 2>/dev/null; do
  if ! controller_is_live; then
    stopped=137
    break
  fi
  if [ -d "$cancel_path" ]; then
    stopped=143
    break
  fi
  sleep 0.05
done
if [ "$stopped" -eq 0 ]; then
  wait "$apt_pid" 2>/dev/null
  status=$?
else
  status=$stopped
fi
trap '' ALRM INT TERM
if [ "$stopped" -ne 0 ]; then
  status=$stopped
fi

if kill -0 -- "-$apt_pid" 2>/dev/null; then
  kill -TERM -- "-$apt_pid" 2>/dev/null || :
  sleep "$grace"
  kill -KILL -- "-$apt_pid" 2>/dev/null || :
fi
wait "$apt_pid" 2>/dev/null || :
if ! (set -C; : > "$closed_path") 2>/dev/null; then
  if [ "$status" -eq 0 ]; then
    exit 1
  fi
  exit "$status"
fi
printf "CLOSED\n" >&3 || exit 1
if [ -d "$cancel_path" ]; then
  # A blocked controller cannot acknowledge socket cancellation. Package work
  # is settled now; retire only that known parent, never a global process name.
  if controller_is_live; then
    kill -KILL "$controller_pid" 2>/dev/null || :
  fi
fi
exit "$status"
`

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

function hasPhaseClosure(closedPath) {
  try {
    const receipt = lstatSync(closedPath)
    return receipt.isFile() && receipt.size === 0
  }
  catch {
    return false
  }
}

async function waitForPhaseClosure(closedPath, deadline) {
  for (;;) {
    if (hasPhaseClosure(closedPath))
      return true

    const remaining = deadline - performance.now()
    if (remaining <= 0)
      return false
    await new Promise(resolve => setTimeout(resolve, Math.min(50, remaining)))
  }
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
    let stoppedCode
    let aptPid
    let ownerClosed = false
    let ownerInput = ''
    let fallbackTimer
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

    function finish(status) {
      if (finished)
        return
      finished = true
      let resultCode = status
      try {
        const closedPath = `${controlPath}.${phase}.closed`
        if (!hasPhaseClosure(closedPath))
          closeSync(openSync(closedPath, 'wx', 0o600))
      }
      catch {
        marker(writeMarker, phase, 'cleanup-unknown')
        resultCode ||= 1
      }

      clearTimeout(fallbackTimer)
      child?.stdio[4]?.destroy()
      process.removeListener('SIGTERM', terminate)
      process.removeListener('SIGINT', interrupt)
      controlSocket?.destroy()
      resolve(resultCode)
    }

    function cleanup(status) {
      if (stopping)
        return
      stopping = true
      stoppedCode = status
      child?.stdio[4]?.end()
      if (!child?.pid) {
        finish(status)
        return
      }

      // Ask the independent privileged guard to stop. It returns only after its
      // group cleanup; killing it here would discard that ownership boundary.
      try {
        child.kill('SIGTERM')
      }
      catch {
        marker(writeMarker, phase, 'cleanup-error', 'signal=SIGTERM')
      }
    }

    process.on('SIGTERM', terminate)
    process.on('SIGINT', interrupt)

    function startApt() {
      const args = phase === 'install' ? [...specification.args, ...profileSpecification.packages] : specification.args
      const graceSeconds = cleanupGraceMs / 1000
      const controllerStat = readFileSync('/proc/self/stat', 'utf8')
      const controllerStarted = controllerStat.slice(controllerStat.lastIndexOf(')') + 2).split(' ')[19]
      if (!/^\d+$/.test(controllerStarted)) {
        cleanup(1)
        return
      }

      child = spawn('timeout', [
        '--preserve-status',
        '--signal=ALRM',
        `--kill-after=${graceSeconds * 2}s`,
        `${limitSeconds}s`,
        'bash',
        '-c',
        aptOwner,
        'cairn-native-apt-owner',
        `${graceSeconds}`,
        `${process.pid}`,
        controllerStarted,
        `${controlPath}.cancel`,
        `${controlPath}.${phase}.closed`,
        'apt-get',
        ...args,
      ], { detached: true, stdio: ['ignore', 'inherit', 'inherit', 'pipe', 'pipe'] })
      // Register the owned group before admitting APT. The independent shell
      // handles a stalled Node loop; Node retains cleanup if that shell dies.
      child.stdio[4].on('error', () => cleanup(1))
      child.stdio[3].on('error', () => cleanup(1))
      child.stdio[3].on('data', (bytes) => {
        ownerInput += bytes
        if (ownerInput.length > 64) {
          cleanup(1)
          return
        }

        while (ownerInput.includes('\n')) {
          const boundary = ownerInput.indexOf('\n')
          const message = ownerInput.slice(0, boundary)
          ownerInput = ownerInput.slice(boundary + 1)
          if (!aptPid && /^[1-9]\d{0,9}$/.test(message)) {
            const reportedPid = Number(message)
            if (reportedPid > 2147483647 || [process.pid, process.ppid, child.pid].includes(reportedPid)) {
              cleanup(1)
              return
            }

            aptPid = reportedPid
            if (!stopping) {
              child.stdio[4].end('START\n')
              marker(writeMarker, phase, 'apt-start', `profile=${profile}`)
            }
          }
          else if (aptPid && message === 'CLOSED' && !ownerClosed) {
            ownerClosed = true
          }
          else {
            cleanup(1)
          }
        }
      })
      child.on('error', () => cleanup(1))
      child.on('close', (status, signal) => {
        const observed = stoppedCode ?? exitCode({ status, signal })
        if (ownerClosed) {
          finish(observed)
          return
        }

        // A lost guard cannot turn an incomplete phase green. Its private
        // registered group is the only cleanup target; no process-name sweep.
        const failed = observed || 1
        if (!aptPid) {
          finish(failed)
          return
        }

        const signalGroup = (signal) => {
          try {
            process.kill(-aptPid, signal)
          }
          catch {
            // The owned group already exited.
          }
        }

        signalGroup('SIGTERM')
        fallbackTimer = setTimeout(() => {
          signalGroup('SIGKILL')
          finish(failed)
        }, cleanupGraceMs)
      })
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
  let phaseAuthorized = false
  const interrupt = (signal) => {
    if (interruptedCode)
      return
    interruptedCode = 128 + constants.signals[signal]
    let cancellationRecorded = false
    if (controlDirectory) {
      try {
        mkdirSync(join(controlDirectory, 'owner.sock.cancel'), { recursive: true })
        cancellationRecorded = true
      }
      catch {
        marker(write, 'control', 'cancel-marker-error')
      }
    }

    controlSocket?.end(`${signal}\n`)
    // Privilege entry might stall before it can consume the control request.
    // Keep the existing timeout supervisor as a bounded fallback in that case.
    // If marker acquisition failed after admission, retain the original work
    // deadline instead of abandoning a possibly blocked privileged owner early.
    if (!cancellationRecorded && controlSocket)
      return
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
      if (interruptedCode) {
        socket.end(`${interruptedCode === 130 ? 'SIGINT' : 'SIGTERM'}\n`)
      }
      else {
        phaseAuthorized = true
        socket.write('START\n')
      }
    })
    await new Promise((resolve, reject) => {
      controlServer.once('error', reject)
      controlServer.listen(controlPath, resolve)
    })
    for (const phase of Object.keys(phases)) {
      if (interruptedCode)
        return interruptedCode
      const seconds = phaseSeconds[phase] ?? profiles[profile].seconds[phase]
      phaseAuthorized = false
      const started = Date.now()
      const phaseDeadline = performance.now() + (Number(seconds) + entryAllowanceSeconds + killAfterSeconds * 2) * 1000
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
      // A failed sudo/controller exit does not prove that its detached guard
      // has settled APT. Retain the existing cleanup allowance, clipped to the
      // original outer phase budget, to acquire independent group closure.
      if (phaseAuthorized) {
        const closedPath = `${controlPath}.${phase}.closed`
        const deadline = Math.min(phaseDeadline, performance.now() + killAfterSeconds * 1000)
        const closed = await waitForPhaseClosure(closedPath, deadline)
        if (!closed) {
          marker(write, phase, 'cleanup-unknown')
          if (result.status === 0)
            result.status = 1
        }
      }

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
