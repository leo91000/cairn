#!/usr/local/bin/node
import { spawn } from 'node:child_process'
import { createHash } from 'node:crypto'
import { createWriteStream } from 'node:fs'
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rename,
  rm,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { Readable, Transform } from 'node:stream'
import { pipeline } from 'node:stream/promises'
import { fileURLToPath } from 'node:url'
import { emulator } from './android-emulator.mjs'

const output = value => process.stdout.write(`${value}\n`)

export const tools = { build: '15859902', sha256: '4e4c464f145a7512b57d088ac6c278c03c9eea610886b35a5e0804e74eedf583' }
export function environment(env = process.env) {
  if (!env.HOME || !path.isAbsolute(env.HOME))
    throw new Error('An absolute persistent HOME is required.')
  return {
    ...env,
    ANDROID_HOME: env.ANDROID_HOME || path.join(env.HOME, '.local/share/android/sdk'),
    ANDROID_USER_HOME: env.ANDROID_USER_HOME || path.join(env.HOME, '.android'),
    GRADLE_USER_HOME: env.GRADLE_USER_HOME || path.join(env.HOME, '.gradle'),
  }
}

export function packages(args) {
  const selected = args.filter(arg => arg !== '--accept-licenses')
  if (selected.some(arg => !/^(?:platform-tools|emulator|platforms;android-\d+|build-tools;\d+\.\d+\.\d+|system-images;android-\d+;(?:default|google_apis);x86_64|ndk;\d+\.\d+\.\d+|cmake;\d+\.\d+\.\d+)$/.test(arg)))
    throw new Error('Use stable SDK package IDs, for example "platforms;android-36" "build-tools;36.0.0".')
  return [...new Set(['platform-tools', ...selected])]
}

export function run(binary, args, env, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(binary, args, { env, stdio: [input == null ? 'inherit' : 'pipe', 'inherit', 'inherit'] })
    child.on('error', reject)
    child.on('exit', (code, signal) => code === 0 ? resolve() : reject(new Error(`${path.basename(binary)} failed (${signal || code}).`)))
    if (input != null) {
      child.stdin.on('error', (error) => {
        if (error.code !== 'EPIPE')
          reject(error)
      })
      child.stdin.end(input)
    }
  })
}

export async function setup(args, env = environment()) {
  const selected = packages(args)
  const sdk = env.ANDROID_HOME
  if (!path.isAbsolute(sdk) || sdk === '/tmp' || sdk.startsWith('/tmp/'))
    throw new Error('ANDROID_HOME must be an absolute persistent directory outside /tmp.')
  await mkdir(sdk, { recursive: true })
  const cli = path.join(sdk, 'cmdline-tools/latest')
  try {
    await access(path.join(cli, 'bin/sdkmanager'))
  }
  catch {
    const staging = await mkdtemp(path.join(sdk, '.install-'))
    try {
      const archive = path.join(staging, 'tools.zip')
      const response = await fetch(`https://dl.google.com/android/repository/commandlinetools-linux-${tools.build}_latest.zip`, { signal: AbortSignal.timeout(300000) })
      if (!response.ok)
        throw new Error(`Android tools download failed: HTTP ${response.status}.`)
      const hash = createHash('sha256')
      let size = 0
      await pipeline(Readable.fromWeb(response.body), new Transform({
        transform(chunk, encoding, callback) {
          size += chunk.length
          hash.update(chunk)
          callback(size > 300_000_000 ? new Error('Android tools download exceeds limit.') : null, chunk)
        },
      }), createWriteStream(archive, { flags: 'wx', mode: 0o600 }))
      if (hash.digest('hex') !== tools.sha256)
        throw new Error('Android command-line tools checksum mismatch.')
      await run('unzip', ['-q', archive, '-d', staging], env)
      await mkdir(path.dirname(cli), { recursive: true })
      await rename(path.join(staging, 'cmdline-tools'), cli)
    }
    finally { await rm(staging, { recursive: true, force: true }) }
  }

  const command = path.join(cli, 'bin/sdkmanager')
  // Run through mise so JAVA_HOME follows project pins, not the manager JDK.
  const yes = args.includes('--accept-licenses') ? 'y\n'.repeat(200) : undefined
  await run('mise', ['exec', '--', command, `--sdk_root=${sdk}`, ...selected], env, yes)
  output(`Android SDK ready at ${sdk}. Use the project's ./gradlew; caches persist between turns.`)
}

async function main() {
  const args = process.argv.slice(2)
  const env = environment()
  if (args[0] === 'status') {
    const exists = file => access(file).then(() => true, () => false)
    output(JSON.stringify({
      sdk: env.ANDROID_HOME,
      sdkInstalled: await exists(path.join(env.ANDROID_HOME, 'cmdline-tools/latest/bin/sdkmanager')),
      kvm: await exists('/dev/kvm'),
      bootId: (await readFile('/proc/sys/kernel/random/boot_id', 'utf8')).trim(),
    }))
    return
  }

  if (args[0] === 'emulator' && ['status', 'stop'].includes(args[1]))
    return emulator(args[1], args.slice(2), env, setup, run)
  if (args[0] === 'emulator' && args[1] === '--locked')
    return emulator(args[2], args.slice(3), env, setup, run)
  if (!['setup', 'emulator'].includes(args[0]))
    throw new Error('Usage: leo-android setup [--accept-licenses] [SDK packages...] | status')
  if (args[0] === 'setup' && args[1] === '--locked')
    return setup(args.slice(2), env)
  const directory = path.join(env.HOME, '.local/share/android')
  await mkdir(directory, { recursive: true })
  // Kernel lock is released on exit/restart; no stale lockfile can block a chat.
  await run('flock', ['-w', '600', path.join(directory, 'bootstrap.lock'), process.execPath, fileURLToPath(import.meta.url), args[0], '--locked', ...args.slice(1)], env)
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error.message)
    process.exitCode = 1
  })
}
