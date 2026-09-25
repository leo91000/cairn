import { execFile, spawn } from 'node:child_process'
import {
  access,
  mkdir,
  open,
  readFile,
  rename,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as sleep } from 'node:timers/promises'
import { promisify } from 'node:util'

const output = value => process.stdout.write(`${value}\n`)

const exec = promisify(execFile)
const port = 5580
const serial = `emulator-${port}`
const stateFile = env => path.join(env.ANDROID_USER_HOME, 'leo-emulator.json')

async function identity(pid) {
  const fields = (await readFile(`/proc/${pid}/stat`, 'utf8')).split(') ').at(-1).split(' ')
  if (fields[0] === 'Z')
    throw new Error('Process exited')
  return { boot: (await readFile('/proc/sys/kernel/random/boot_id', 'utf8')).trim(), start: fields[19] }
}

async function running(env) {
  try {
    const state = JSON.parse(await readFile(stateFile(env), 'utf8'))
    if (!Number.isInteger(state.pid) || state.pid <= 1)
      return null
    const current = await identity(state.pid)
    return current.boot === state.boot && current.start === state.start ? state : null
  }
  catch { return null }
}

async function adb(env, args) {
  const result = await exec(path.join(env.ANDROID_HOME, 'platform-tools/adb'), ['-s', serial, ...args], { env, timeout: 10000, maxBuffer: 1024 * 1024 })
  return result.stdout.trim()
}

export function apiLevel(value) {
  if (!/^\d{2}$/.test(value || '') || Number(value) < 29 || Number(value) > 99)
    throw new Error('Choose an Android API level between 29 and 99.')
  return value
}

export async function emulator(action, args, env, setup, run) {
  const current = await running(env)
  if (action === 'status') {
    const booted = current && await adb(env, ['shell', 'getprop', 'sys.boot_completed']).catch(() => '') === '1'
    output(JSON.stringify({
      state: booted ? 'ready' : current ? 'booting' : 'stopped',
      serial: current ? serial : null,
      acceleration: current?.acceleration,
      api: current?.api,
      image: current ? current.image || 'google-apis' : null,
    }))
    return
  }

  if (action === 'stop') {
    if (current) {
      await adb(env, ['emu', 'kill']).catch(() => {})
      // The boot + process start identity prevents PID reuse after a VM restart.
      if (await running(env))
        process.kill(current.pid, 'SIGTERM')
      for (let attempt = 0; attempt < 50 && await running(env); attempt++)
        await sleep(100)
      if (await running(env))
        process.kill(current.pid, 'SIGKILL')
    }

    output('Android emulator stopped. Device data is preserved.')
    return
  }

  if (action !== 'start')
    throw new Error('Usage: leo-android emulator start <API> [--aosp] [--accept-licenses] | status | stop')
  const api = apiLevel(args[0])
  if (args.slice(1).some(arg => !['--accept-licenses', '--aosp'].includes(arg)))
    throw new Error('Unknown emulator option.')
  const image = args.includes('--aosp') ? 'aosp' : 'google-apis'
  if (current && (current.api !== api || (current.image || 'google-apis') !== image))
    throw new Error(`API ${current.api} (${current.image || 'google-apis'}) is already running. Stop it before switching devices.`)
  let state = current
  if (!state) {
    const system = `system-images;android-${api};${image === 'aosp' ? 'default' : 'google_apis'};x86_64`
    await setup(['emulator', system, ...args.slice(1).filter(arg => arg === '--accept-licenses')], env)
    await mkdir(env.ANDROID_USER_HOME, { recursive: true })
    const avd = `leo_api_${api}${image === 'aosp' ? '_aosp' : ''}`
    const avdHome = path.join(env.ANDROID_USER_HOME, 'avd')
    env = { ...env, ANDROID_AVD_HOME: avdHome }
    await mkdir(avdHome, { recursive: true })
    try {
      await access(path.join(avdHome, `${avd}.ini`))
    }
    catch {
      await run('mise', ['exec', '--', path.join(env.ANDROID_HOME, 'cmdline-tools/latest/bin/avdmanager'), 'create', 'avd', '--name', avd, '--package', system, '--device', 'pixel_6'], env, 'no\n')
      const configPath = path.join(avdHome, `${avd}.avd/config.ini`)
      const config = await readFile(configPath, 'utf8')
      const display = { 'hw.lcd.width': '720', 'hw.lcd.height': '1280', 'hw.lcd.density': '320' }
      const lines = config.split('\n').filter(line => !Object.hasOwn(display, line.split('=')[0].trim()))
      await writeFile(configPath, [...lines, ...Object.entries(display).map(([key, value]) => `${key}=${value}`)].join('\n'))
    }

    const acceleration = await access('/dev/kvm', 6).then(() => 'kvm', () => 'software')
    const logPath = path.join(env.ANDROID_USER_HOME, 'leo-emulator.log')
    const log = await open(logPath, 'w', 0o600)
    const child = spawn(path.join(env.ANDROID_HOME, 'emulator/emulator'), ['-avd', avd, '-port', `${port}`, '-accel', acceleration === 'kvm' ? 'on' : 'off', '-gpu', 'swiftshader', '-feature', '-Vulkan', '-skin', '720x1280', '-memory', '1536', '-cores', '2', '-no-window', '-no-audio', '-no-snapshot', '-no-boot-anim', '-no-metrics'], { env, detached: true, stdio: ['ignore', log.fd, log.fd] })
    await new Promise((resolve, reject) => {
      child.once('spawn', resolve)
      child.once('error', reject)
    })
    await log.close()
    child.unref()
    state = {
      pid: child.pid,
      ...await identity(child.pid),
      serial,
      api,
      image,
      acceleration,
      logPath,
    }
    const temporary = `${stateFile(env)}.tmp`
    await writeFile(temporary, JSON.stringify(state), { mode: 0o600 })
    await rename(temporary, stateFile(env))
    output(`Booting Android API ${api} (${acceleration}). Log: ${logPath}`)
  }

  // Nested KVM on older hosts can exceed three minutes even while boot progresses.
  const deadline = Date.now() + 600000
  while (Date.now() < deadline) {
    if (!await running(env))
      throw new Error(`Emulator exited. Inspect ${state.logPath}.`)
    if (await adb(env, ['shell', 'getprop', 'sys.boot_completed']).catch(() => '') === '1') {
      output(JSON.stringify({
        state: 'ready',
        serial,
        api,
        image: state.image || 'google-apis',
        acceleration: state.acceleration,
        adb: `adb -s ${serial}`,
      }))
      return
    }

    await sleep(2000)
  }

  if (await running(env))
    process.kill(state.pid, 'SIGTERM')
  throw new Error(`Android boot timed out (${state.acceleration}); emulator stopped. Inspect ${state.logPath}. JVM tests are not a replacement for device validation.`)
}
