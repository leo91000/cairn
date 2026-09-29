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
      await writeFile(path.join(env.ANDROID_USER_HOME, 'leo-emulator.json'), JSON.stringify({ pid: process.pid, boot: 'earlier-VM-boot', start: '1' }))
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
