import type { Buffer } from 'node:buffer'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'

const execute = promisify(execFile)

export function createNativeAndroidDevice(binary: string, serial: string | undefined) {
  function device() {
    if (!serial?.startsWith('emulator-'))
      throw new Error('This journey needs ANDROID_SERIAL selecting a disposable emulator.')
    return ['-s', serial]
  }

  return {
    async command(...args: string[]): Promise<string> {
      const { stdout } = await execute(binary, [...device(), ...args], { encoding: 'utf8', timeout: 15000 })
      return stdout
    },
    async screenshot(): Promise<Buffer> {
      const { stdout } = await execute(binary, [...device(), 'exec-out', 'screencap', '-p'], { encoding: 'buffer', timeout: 15000 })
      return stdout
    },
  }
}
