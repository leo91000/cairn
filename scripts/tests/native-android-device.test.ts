import { once } from 'node:events'
import { mkdtemp, rm, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import { expect, it } from 'vitest'
import { createNativeAndroidDevice } from '../../tests/e2e/native-android-device'

it('lets a fixture server answer while a native command is waiting for it', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'native-adb-'))
  const binary = path.join(directory, 'adb.cjs')
  let requests = 0
  const server = createServer((_request, response) => {
    requests++
    response.end('fixture-ready')
  })
  try {
    server.listen(0, '127.0.0.1')
    await once(server, 'listening')
    const address = server.address()
    if (!address || typeof address === 'string')
      throw new Error('Missing test server address')
    await writeFile(binary, `#!/usr/bin/env node
require('node:http').get(process.argv.at(-1), response => {
  response.pipe(process.stdout)
}).on('error', error => { console.error(error.message); process.exitCode = 1 })
`, { mode: 0o700 })
    const device = createNativeAndroidDevice(binary, 'emulator-5582')
    expect(await device.command('fixture', `http://127.0.0.1:${address.port}/`)).toBe('fixture-ready')
    expect(requests).toBe(1)
  }
  finally {
    server.closeAllConnections()
    await new Promise<void>(resolve => server.close(() => resolve()))
    await rm(directory, { recursive: true, force: true })
  }
}, 20000)

it('refuses commands on an unspecified or physical device without blocking test discovery', async () => {
  await expect(createNativeAndroidDevice('unused', undefined).command('unused')).rejects.toThrow('disposable emulator')
  await expect(createNativeAndroidDevice('unused', 'physical-device').screenshot()).rejects.toThrow('disposable emulator')
})
