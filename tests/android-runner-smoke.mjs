// Explicit opt-in integration probe: downloads an Android system image and boots
// a real KVM-accelerated device twice inside a disposable Firecracker run.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as sleep } from 'node:timers/promises'
import { prepareStorageOrigin } from './runner-storage-smoke.mjs'

async function main() {
  const image = process.argv[2] || 'leo-audit:test'
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || '/var/tmp', 'leo-android-vm-'))
  const name = `leo-android-test-${randomUUID().slice(0, 8)}`
  const docker = (...args) => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 180000 }).trim()
  const results = []
  let evidence
  if (process.env.ANDROID_EVIDENCE) {
    assert.notEqual(process.env.KEEP_VM_TEST, '1', 'Qualification requires cleanup')
    assert.match(image, /^ghcr\.io\/[\w.-]+\/[\w.-]+@sha256:[a-f0-9]{64}$/)
    const cpu = await readFile('/proc/cpuinfo', 'utf8')
    assert.match(cpu, /vendor_id\s*:\s*GenuineIntel/)
    assert.equal(process.env.ANDROID_TEST_SYSTEM, 'aosp')
    assert.equal(process.env.ANDROID_TEST_API || '34', '34')
    assert.match(process.env.ANDROID_TEST_COMMIT || '', /^[a-f0-9]{40}$/)
    docker('pull', image)
    const labels = JSON.parse(docker('image', 'inspect', '--format', '{{json .Config.Labels}}', image))
    assert.equal(labels['org.opencontainers.image.revision'], process.env.ANDROID_TEST_COMMIT)
    evidence = { schema: 1, repository: image.slice('ghcr.io/'.length).split('@')[0], image, commit: process.env.ANDROID_TEST_COMMIT, cpuVendor: 'GenuineIntel', api: '34', system: 'aosp', results }
  }
  const runId = randomUUID()
  const runRoot = `/data/runs/${runId}`
  let url
  const headers = { 'authorization': 'Bearer fixture-android-token', 'content-type': 'application/json' }
  async function api(route, method = 'GET', body) {
    const result = await fetch(url + route, { method, headers, body: body && JSON.stringify(body), signal: AbortSignal.timeout(1800000) })
    assert.ok(result.ok, `${route}: ${result.status}`)
    return result
  }
  async function until(fn, timeout = 180000) {
    const deadline = Date.now() + timeout
    while (Date.now() < deadline) {
      if (await fn())
        return
      await sleep(200)
    }
    throw new Error('Android VM probe timed out')
  }
  try {
    const source = path.join(root, 'data/runs', runId)
    for (const dir of ['data/runner-plans', 'state', `data/runs/${runId}/workspace`, `data/runs/${runId}/home`, `data/runs/${runId}/output`])
      await mkdir(path.join(root, dir), { recursive: true })
    await writeFile(path.join(root, 'data/runner-secret'), 'fixture-android-token')
    await writeFile(path.join(source, 'workspace/probe.mjs'), await readFile(new URL('./fixtures/android-device-probe.mjs', import.meta.url)))
    await writeFile(path.join(source, 'workspace/nested-kvm.c'), await readFile(new URL('./fixtures/nested-kvm.c', import.meta.url)))
    docker('run', '-d', '--name', name, '--user', '0:0', '--read-only', '--cap-drop', 'ALL', ...['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'].flatMap(cap => ['--cap-add', cap]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', '--device', '/dev/kvm', '--device', '/dev/net/tun', '--device', '/dev/fuse', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', '7g', '--cpus', '3', '--entrypoint', '/usr/local/bin/leo', image, 'runner-broker')
    url = `http://${docker('port', name, '4311/tcp')}`
    await until(() => fetch(`${url}/health`).then(r => r.ok).catch(() => false))
    // This device probe has no manager publishing while the SDK is installed.
    const { storage } = await prepareStorageOrigin({ root, docker, name, api, maxDirtySeconds: 3600 })
    for (const mode of ['first', 'resume']) {
      const id = randomUUID()
      const ackId = randomUUID()
      const plan = { id, runId, storage, expires: Date.now() + 1800000, sandbox: 'yolo', cwd: `${runRoot}/workspace`, command: ['/usr/local/bin/node', `${runRoot}/workspace/probe.mjs`, mode, ackId, process.env.ANDROID_TEST_API || '34', process.env.ANDROID_TEST_SYSTEM || 'google-apis'], chat: { output: `${runRoot}/output/result.md` }, imports: ['workspace', 'home', 'output'].map(dir => ({ source: `${runRoot}/${dir}`, target: dir === 'home' ? '/home/node' : `${runRoot}/${dir}`, readOnly: false })) }
      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      const started = Date.now()
      await api(`/runs/${id}`, 'POST')
      const response = await api(`/runs/${id}/logs`)
      let pending = ''
      let output = ''
      for await (const chunk of response.body) {
        pending += Buffer.from(chunk).toString()
        while (pending.includes('\n')) {
          const end = pending.indexOf('\n')
          const row = JSON.parse(pending.slice(0, end))
          pending = pending.slice(end + 1)
          if (row.type !== 'output')
            continue
          const text = Buffer.from(row.data, 'base64').toString()
          output += text
          process.stdout.write(text)
          if (text.includes('probe.capture')) {
            const capture = await api(`/runs/${id}/artifact`, 'POST', { runId, path: `${runRoot}/workspace/device.png` })
            const png = Buffer.from(await capture.arrayBuffer())
            assert.equal(png.subarray(1, 4).toString(), 'PNG')
            const screenshots = process.env.ANDROID_SCREENSHOTS || '/var/tmp/leo-android-screenshots'
            await mkdir(screenshots, { recursive: true })
            await writeFile(path.join(screenshots, `${mode}.png`), png)
            // Export is acknowledged by an on-demand project import, never by mounting the guest disk.
            const marker = `${runRoot}/workspace/${ackId}`
            await mkdir(path.join(source, 'workspace', ackId), { recursive: true })
            await writeFile(path.join(source, 'workspace', ackId, 'ok'), '1')
            await api(`/runs/${id}/projects/${ackId}`, 'POST', { runId, source: marker, target: marker })
          }
        }
      }
      const result = await (await api(`/runs/${id}/wait`, 'POST')).json()
      assert.equal(result.StatusCode, 0, output.slice(-6000))
      assert.ok(output.includes('probe.done'))
      const passed = { mode, durationMs: Date.now() - started, status: 'passed' }
      results.push(passed)
      process.stdout.write(`${JSON.stringify(passed)}\n`)
    }
  }
  catch (error) {
    process.stderr.write(docker('logs', name))
    process.stderr.write(docker('exec', name, 'sh', '-c', 'tail -n 50 /runner-state/*.boot.log'))
    throw error
  }
  finally {
    if (process.env.KEEP_VM_TEST === '1') {
      process.stdout.write(`Evidence: ${root}, container: ${name}\n`)
    }
    else {
      try {
        docker('stop', name)
        docker('rm', name)
      }
      catch {}
      docker('run', '--rm', '--user', '0:0', '-v', `${root}:/cleanup`, '--entrypoint', '/bin/rm', image, '-r', '/cleanup/data', '/cleanup/state')
      await rm(root, { recursive: true, force: true })
    }
  }
  if (evidence)
    await writeFile(process.env.ANDROID_EVIDENCE, JSON.stringify({ ...evidence, completedAt: new Date().toISOString() }, null, 2))
}
main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
