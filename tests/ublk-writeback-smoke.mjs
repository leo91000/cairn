// Opt-in kernel integration: node tests/ublk-writeback-smoke.mjs IMAGE PROBE
// Build a Debian-compatible PROBE with --features ublk-prototype --example ublk_probe.
// Requires a loaded ublk driver. Raw evidence stays in /var/tmp, outside Git.
import assert from 'node:assert/strict'
import { execFileSync, spawnSync } from 'node:child_process'
import fs from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'

const docker = args => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 45000, maxBuffer: 1024 * 1024 })
const blockDevices = async () => (await fs.readdir('/sys/class/block')).filter(name => name.startsWith('ublkb')).sort()

async function dispose(root, name, image, started, initial) {
  if (started) {
    docker(['stop', '--time', '3', name])
    docker(['rm', '-v', name])
  }

  assert.deepEqual(await blockDevices(), initial, 'Devices remain; preserve the fixture for recovery')
  docker(['run', '--rm', '--network', 'none', '--read-only', '--user', '0', '--cap-drop', 'ALL', '--cap-add', 'DAC_OVERRIDE', '--memory', '64m', '-v', `${root}:/cleanup`, '--entrypoint', 'rm', image, '-rf', '/cleanup/state', '/cleanup/journal'])
  await fs.rm(root, { recursive: true, force: true })
}

async function trial(image, probe, mode, directory, majors) {
  const root = await fs.mkdtemp('/var/tmp/leo-ublk-writeback-')
  const name = `leo-ublk-writeback-${path.basename(root).split('-').at(-1).toLowerCase()}`
  const prefix = `${directory}/${mode}`
  await fs.mkdir(`${root}/state`)
  await fs.mkdir(`${root}/fixtures`)
  await fs.copyFile(path.resolve(probe), `${root}/fixtures/ublk_probe`)
  await fs.copyFile(new URL('./fixtures/ublk-writeback.mjs', import.meta.url), `${root}/fixtures/inner.mjs`)
  const base = await fs.open(`${root}/base`, 'wx')
  await base.truncate(1024 ** 3)
  await base.close()
  const initial = await blockDevices()
  await fs.writeFile(`${prefix}-owner.json`, JSON.stringify({
    root,
    name,
    image,
    probe: path.resolve(probe),
    mode,
    initial,
    majors,
    at: new Date().toISOString(),
    hostIoPressure: await fs.readFile('/proc/pressure/io', 'utf8'),
    hostMemory: (await fs.readFile('/proc/meminfo', 'utf8')).split('\n').filter(line => /^(?:Dirty|Writeback|MemAvailable):/.test(line)),
  }, null, 2))
  const caps = ['SYS_ADMIN', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE', ...(mode === 'with-cap' ? ['SYS_RESOURCE'] : [])]
  let started = false
  try {
    docker(['run', '-d', '--name', name, '--network', 'none', '--user', '0', '--read-only', '--cap-drop', 'ALL', ...caps.flatMap(cap => ['--cap-add', cap]), '--device', '/dev/ublk-control', '--device-cgroup-rule', `c ${majors.c}:* rwm`, '--device-cgroup-rule', `b ${majors.b}:* rwm`, '--security-opt', 'seccomp=unconfined', '--security-opt', 'apparmor=unconfined', '--tmpfs', '/run', '--tmpfs', '/tmp', '--memory', '1280m', '--cpus', '2', '--pids-limit', '64', '-v', `${root}:${root}`, '-v', `${root}/fixtures:/fixtures:ro`, '-e', `LEO_WRITEBACK_FIXTURE=${root}`, '-e', `LEO_FLUSHER_MODE=${mode}`, '--entrypoint', 'node', image, '-e', 'setInterval(()=>{},1000)'])
    started = true
    const child = spawnSync('docker', ['--context', 'default', 'exec', name, 'node', '/fixtures/inner.mjs'], { encoding: 'utf8', timeout: 45000, maxBuffer: 2 * 1024 ** 2 })
    await fs.writeFile(`${prefix}.log`, `${child.stdout ?? ''}\n${child.stderr ?? ''}`)
    await fs.writeFile(`${prefix}-process.json`, JSON.stringify({
      status: child.status,
      signal: child.signal,
      error: child.error?.message,
      container: JSON.parse(docker(['inspect', name]))[0].State,
    }, null, 2))
    for (const file of ['backend.log', 'result.json', 'cleanup.json'])
      await fs.copyFile(`${root}/${file}`, `${prefix}-${file}`)
    const result = JSON.parse(await fs.readFile(`${prefix}-result.json`, 'utf8'))
    if (mode === 'no-cap') {
      assert.equal(child.status, 1)
      assert.match(result.error, /CAP_SYS_RESOURCE/)
      assert.equal(result.device, undefined, 'Missing permission fails before publishing a device')
    }
    else {
      assert.equal(child.status, 0, result.error)
      assert.equal(result.timedOut, false, 'Buffered writes must make progress')
      assert.equal(result.exit.code, 0)
      assert.equal(result.verifiedAfterCrashBytes, 1024 * 1024 ** 2)
      assert.ok(result.finalBeforeCrash.metrics.committedFrames >= 2052)
      assert.ok(result.finalBeforeCrash.metrics.maxCommitFrames > 1, 'Durable writes remain grouped')
      for (const sample of [result.finalBeforeCrash, result.finalAfterCrash]) {
        assert.match(sample.memoryEvents, /^oom 0$/m)
        assert.match(sample.memoryEvents, /^oom_kill 0$/m)
      }
    }

    assert.equal(JSON.parse(await fs.readFile(`${prefix}-cleanup.json`, 'utf8')).status, 0)
    process.stdout.write(`${JSON.stringify({
      mode,
      status: 'passed',
      ioDurationMs: result.ioDurationMs,
      verifiedAfterCrashBytes: result.verifiedAfterCrashBytes,
      evidence: `${prefix}-result.json`,
    })}\n`)
  }
  finally {
    await dispose(root, name, image, started, initial)
  }
}

async function main() {
  const [image, probe] = process.argv.slice(2)
  assert.ok(image && probe, 'Expected IMAGE and a Debian-compatible ublk_probe binary')
  const majors = {}
  let kind
  for (const line of (await fs.readFile('/proc/devices', 'utf8')).split('\n')) {
    if (line === 'Character devices:') {
      kind = 'c'
      continue
    }

    if (line === 'Block devices:') {
      kind = 'b'
      continue
    }

    const match = line.match(/^\s*(\d+)\s+(\S+)$/)
    if (!match)
      continue
    if (kind === 'c' && match[2] === 'ublk-char')
      majors.c = match[1]
    if (kind === 'b' && ['ublk', 'blkext'].includes(match[2]))
      majors.b = match[1]
  }

  assert.ok(majors.c && majors.b, 'ublk unavailable')
  const directory = await fs.mkdtemp('/var/tmp/leo-ublk-evidence-')
  for (const mode of ['with-cap', 'no-cap'])
    await trial(image, probe, mode, directory, majors)
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
