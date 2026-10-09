import { describe, expect, it } from 'vitest'
import { environment, packages } from '../deploy/toolkit/android.mjs'
import { newerTool, validVersion } from '../deploy/toolkit/versions.mjs'

describe('persistent Android tooling', () => {
  it('keeps SDK, Gradle and emulator state inside the run home', () => {
    const env = environment({ HOME: '/home/agent' })
    expect(env.ANDROID_HOME).toBe('/home/agent/.local/share/android/sdk')
    expect(env.ANDROID_USER_HOME).toBe('/home/agent/.android')
    expect(env.GRADLE_USER_HOME).toBe('/home/agent/.gradle')
    expect(() => environment({ HOME: 'relative' })).toThrow(/absolute/)
    expect(environment({ HOME: '/home/agent', ANDROID_HOME: '/persistent/sdk' }).ANDROID_HOME).toBe('/persistent/sdk')
  })
  it('installs only requested stable SDK packages', () => {
    expect(packages(['--accept-licenses', 'platforms;android-36', 'build-tools;36.0.0', 'platform-tools'])).toEqual(['platform-tools', 'platforms;android-36', 'build-tools;36.0.0'])
    for (const option of ['--update', '--sdk_root=/tmp/foo', '../foo', 'system-images;android-36;google_apis;x86_64;evil'])
      expect(() => packages([option])).toThrow(/SDK package/)
  })
  it('compares Temurin patch and build releases without downgrading or accepting EA', () => {
    const old = 'temurin-21.0.11+10.0.LTS'
    const next = 'temurin-21.0.12+101.0.LTS'
    expect(newerTool('java', old, next)).toBe(next)
    expect(newerTool('java', next, old)).toBe(next)
    expect(newerTool('java', 'temurin-21.0.12+9.0.LTS', 'temurin-21.0.12+10.0.LTS')).toBe('temurin-21.0.12+10.0.LTS')
    expect(validVersion('java', 'temurin-22-ea')).toBe(false)
    expect(validVersion('java', '21.0.12')).toBe(false)
    expect(newerTool('node', '24.21.0', '24.20.0')).toBe('24.21.0')
  })
})

describe('device process ownership across VM restarts', () => {
  it.each([0, 1])('lets Android persist state when the shutdown connection exits with %i', async (adbExit) => {
    const { spawn } = await import('node:child_process')
    const {
      mkdtemp,
      mkdir,
      readFile,
      writeFile,
      rm,
    } = await import('node:fs/promises')
    const { tmpdir } = await import('node:os')
    const { default: path } = await import('node:path')
    const { default: process } = await import('node:process')
    const { emulator } = await import('../deploy/toolkit/android-emulator.mjs')
    const home = await mkdtemp(path.join(tmpdir(), 'android-shutdown-'))
    const env = environment({ HOME: home, PATH: process.env.PATH })
    const saved = path.join(home, 'device-state-saved')
    const child = spawn(process.execPath, ['-e', `
process.on('SIGUSR1', () => setTimeout(() => {
  require('node:fs').writeFileSync(${JSON.stringify(saved)}, 'saved');
  process.exit(0);
}, 200));
setInterval(() => {}, 1000);
process.send('ready');
`], { stdio: ['ignore', 'ignore', 'ignore', 'ipc'] })
    const exited = new Promise(resolve => child.once('exit', resolve))
    try {
      await new Promise(resolve => child.once('message', resolve))
      await mkdir(env.ANDROID_USER_HOME, { recursive: true })
      await mkdir(path.join(env.ANDROID_HOME, 'platform-tools'), { recursive: true })
      const boot = (await readFile('/proc/sys/kernel/random/boot_id', 'utf8')).trim()
      const start = (await readFile(`/proc/${child.pid}/stat`, 'utf8')).split(') ').at(-1).split(' ')[19]
      await writeFile(path.join(env.ANDROID_USER_HOME, 'cairn-emulator.json'), JSON.stringify({ pid: child.pid, boot, start }))
      await writeFile(path.join(env.ANDROID_HOME, 'platform-tools/adb'), `#!/usr/bin/env node
const graceful = process.argv.slice(4).join(' ') === 'shell reboot -p';
process.kill(${child.pid}, graceful ? 'SIGUSR1' : 'SIGTERM');
process.exitCode = graceful ? ${adbExit} : 0;
`, { mode: 0o755 })
      await emulator('stop', [], env)
      await exited
      expect(await readFile(saved, 'utf8')).toBe('saved')
    }
    finally {
      if (child.exitCode === null && child.signalCode === null)
        child.kill()
      await exited
      await rm(home, { recursive: true })
    }
  })

  it('never signals a reused PID from an earlier VM boot', async () => {
    const {
      mkdtemp,
      mkdir,
      writeFile,
      rm,
    } = await import('node:fs/promises')
    const { tmpdir } = await import('node:os')
    const { default: path } = await import('node:path')
    const { default: process } = await import('node:process')
    const { vi } = await import('vitest')
    const { emulator } = await import('../deploy/toolkit/android-emulator.mjs')
    const home = await mkdtemp(path.join(tmpdir(), 'android-owner-'))
    const env = environment({ HOME: home })
    const kill = vi.spyOn(process, 'kill').mockReturnValue(true)
    const output = vi.spyOn(process.stdout, 'write').mockReturnValue(true)
    try {
      await mkdir(env.ANDROID_USER_HOME)
      await writeFile(path.join(env.ANDROID_USER_HOME, 'cairn-emulator.json'), JSON.stringify({ pid: process.pid, boot: 'earlier-VM-boot', start: '1' }))
      await emulator('stop', [], env)
      expect(kill).not.toHaveBeenCalled()
    }
    finally {
      kill.mockRestore()
      output.mockRestore()
      await rm(home, { recursive: true })
    }
  })
})

describe('android system image selection', () => {
  it('supports an AOSP device without passing launcher options to the SDK installer', async () => {
    const { emulator } = await import('../deploy/toolkit/android-emulator.mjs')
    const selected = []
    const stop = new Error('SDK installer boundary reached')
    const env = environment({ HOME: '/nonexistent/android-image-test' })
    const setup = async (args) => {
      selected.push(...args)
      throw stop
    }

    await expect(emulator('start', ['34', '--aosp', '--accept-licenses'], env, setup)).rejects.toBe(stop)
    expect(selected).toEqual(['emulator', 'system-images;android-34;default;x86_64', '--accept-licenses'])
  })
  it('retains Google APIs for existing launch commands', async () => {
    const { emulator } = await import('../deploy/toolkit/android-emulator.mjs')
    const selected = []
    const stop = new Error('SDK installer boundary reached')
    const env = environment({ HOME: '/nonexistent/android-image-test' })
    const setup = async (args) => {
      selected.push(...args)
      throw stop
    }

    await expect(emulator('start', ['34', '--accept-licenses'], env, setup)).rejects.toBe(stop)
    expect(selected).toEqual(['emulator', 'system-images;android-34;google_apis;x86_64', '--accept-licenses'])
  })
})
