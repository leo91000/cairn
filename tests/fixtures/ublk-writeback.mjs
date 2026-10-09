import { Buffer } from 'node:buffer'
import { spawn, spawnSync } from 'node:child_process'
import fs from 'node:fs'
import process from 'node:process'
import { setTimeout as sleep } from 'node:timers/promises'

async function main() {
  const root = process.env.CAIRN_WRITEBACK_FIXTURE
  if (!/^\/var\/tmp\/cairn-ublk-writeback-[a-zA-Z0-9]+$/.test(root))
    throw new Error('Invalid fixture')
  const log = fs.openSync(`${root}/backend.log`, 'w')
  const args = ['ublk-managed', `${root}/base`, `${root}/journal`, `${root}/ready`, `${root}/metrics`, `${root}/state`]
  const launch = () => spawn('/fixtures/ublk_probe', args, { stdio: ['ignore', log, log] })
  let backend = launch()
  let writer
  const result = { mode: process.env.CAIRN_FLUSHER_MODE, startedAt: Date.now(), samples: [] }
  const sample = () => ({
    atMs: Date.now() - result.startedAt,
    metrics: JSON.parse(fs.readFileSync(`${root}/metrics`, 'utf8')),
    memoryEvents: fs.readFileSync('/sys/fs/cgroup/memory.events', 'utf8'),
  })
  const cleanup = () => spawnSync('/fixtures/ublk_probe', ['cleanup-managed', `${root}/state`], { encoding: 'utf8', timeout: 10000 })

  async function waitReady() {
    for (let count = 0; !fs.existsSync(`${root}/ready`); count++) {
      if (count > 500 || backend.exitCode !== null)
        throw new Error(`Backend not ready: ${fs.readFileSync(`${root}/backend.log`, 'utf8')}`)
      await sleep(20)
    }
  }

  try {
    await waitReady()
    result.device = fs.readFileSync(`${root}/ready`, 'utf8')
    await sleep(300)
    const started = Date.now()
    writer = spawn('dd', ['if=/dev/zero', `of=${result.device}`, 'bs=1M', 'count=1024', 'conv=fsync', 'status=none'], { stdio: ['ignore', 'pipe', 'pipe'] })
    let output = ''
    writer.stderr.on('data', bytes => output += bytes.toString())
    const completion = new Promise(resolve => writer.once('exit', (code, signal) => resolve({ code, signal })))
    let finished = false
    completion.then(() => finished = true)
    for (let count = 0; count < 200; count++) {
      if (finished)
        break
      if (count % 10 === 0) {
        const current = sample()
        current.waits = fs.readdirSync(`/proc/${backend.pid}/task`).map(tid => ({ tid, wait: fs.readFileSync(`/proc/${backend.pid}/task/${tid}/wchan`, 'utf8') }))
        result.samples.push(current)
      }

      await sleep(100)
    }

    result.timedOut = !finished
    result.ioDurationMs = Date.now() - started
    result.output = output
    if (finished) {
      result.exit = await completion
      if (result.exit.code === 0) {
        const offsets = [0, 128 * 1024 ** 2, 256 * 1024 ** 2, 1024 * 1024 ** 2 - 4096]
        const descriptor = fs.openSync(result.device, 'r+')
        const marker = Buffer.alloc(4096, 0xA7)
        for (const offset of offsets)
          fs.writeSync(descriptor, marker, 0, marker.length, offset)
        fs.fsyncSync(descriptor)
        fs.closeSync(descriptor)
        await sleep(300)
        result.finalBeforeCrash = sample()
        const exited = new Promise(resolve => backend.once('exit', resolve))
        backend.kill('SIGKILL')
        await exited
        await sleep(200)
        const crashCleanup = cleanup()
        result.crashCleanup = { status: crashCleanup.status, stdout: crashCleanup.stdout, stderr: crashCleanup.stderr }
        if (crashCleanup.status !== 0)
          throw new Error('Crash cleanup failed')
        fs.unlinkSync(`${root}/ready`)
        backend = launch()
        await waitReady()
        const restored = fs.openSync(fs.readFileSync(`${root}/ready`, 'utf8'), 'r')
        const chunk = Buffer.alloc(1024 ** 2)
        for (let offset = 0; offset < 1024 * 1024 ** 2; offset += chunk.length) {
          const expected = Buffer.alloc(chunk.length)
          for (const markerOffset of offsets) {
            if (markerOffset >= offset && markerOffset < offset + chunk.length)
              expected.fill(0xA7, markerOffset - offset, markerOffset - offset + 4096)
          }

          if (fs.readSync(restored, chunk, 0, chunk.length, offset) !== chunk.length || !chunk.equals(expected))
            throw new Error(`Journal crash integrity mismatch at ${offset}`)
        }

        fs.closeSync(restored)
        result.verifiedAfterCrashBytes = 1024 * 1024 ** 2
        await sleep(300)
        result.finalAfterCrash = sample()
      }
    }
  }
  catch (error) {
    result.error = error.message
    process.exitCode = 1
  }
  finally {
    fs.writeFileSync(`${root}/result.json`, JSON.stringify(result, null, 2))
    writer?.kill('SIGKILL')
    backend.kill('SIGKILL')
    await sleep(200)
    const cleaned = cleanup()
    fs.writeFileSync(`${root}/cleanup.json`, JSON.stringify({
      status: cleaned.status,
      error: cleaned.error?.message,
      stdout: cleaned.stdout,
      stderr: cleaned.stderr,
    }, null, 2))
    fs.closeSync(log)
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
