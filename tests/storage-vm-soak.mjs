// Opt-in sustained qualification against a disposable real Firecracker runner.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { storageSmoke } from './runner-storage-smoke.mjs'

async function main() {
  const image = process.argv[2]
  assert.ok(image, 'Usage: LEO_STORAGE_SOAK_SECONDS=300 node tests/storage-vm-soak.mjs IMAGE')
  assert.ok(Number(process.env.LEO_STORAGE_SOAK_SECONDS) >= 60)
  const root = await mkdtemp(path.join(os.tmpdir(), 'leo-storage-qualification-'))
  const name = `leo-storage-${randomUUID().slice(0, 8)}`
  let started = false

  function docker(...args) {
    return execFileSync('docker', ['--context', 'default', ...args], {
      encoding: 'utf8',
      timeout: 180000,
      stdio: ['pipe', 'pipe', 'pipe'],
    }).trim()
  }

  async function until(operation, timeout = 180000) {
    const deadline = Date.now() + timeout
    while (Date.now() < deadline) {
      const result = await operation()
      if (result)
        return result
      await setTimeout(100)
    }

    throw new Error('Storage VM qualification timed out')
  }

  try {
    for (const directory of ['data/runner-plans', 'state'])
      await mkdir(path.join(root, directory), { recursive: true })
    await writeFile(path.join(root, 'data/runner-secret'), 'fixture-runner-token')
    const capabilities = ['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE']
    docker('run', '-d', '--name', name, '--user', '0:0', '--read-only', '--cap-drop', 'ALL', ...capabilities.flatMap(capability => ['--cap-add', capability]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', '--device', '/dev/kvm', '--device', '/dev/fuse', '--device', '/dev/net/tun', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', '6g', '--cpus', '3', '-e', 'CONCURRENCY=1', '--entrypoint', '/usr/local/bin/leo', image, 'runner-broker')
    started = true
    const address = docker('port', name, '4311/tcp').split('\n').find(value => value.startsWith('127.0.0.1:'))
    const api = async (endpoint, method = 'GET', body) => {
      const response = await fetch(`http://${address}${endpoint}`, {
        method,
        headers: { 'authorization': 'Bearer fixture-runner-token', 'content-type': 'application/json' },
        body: body ? JSON.stringify(body) : undefined,
        signal: AbortSignal.timeout(180000),
      })
      assert.ok(response.ok, `${method} ${endpoint}: ${response.status}`)
      return response
    }

    await until(async () => {
      try {
        return (await api('/health')).ok
      }
      catch { return false }
    })
    await storageSmoke({
      root,
      docker,
      name,
      api,
      until,
    })
  }
  catch (error) {
    try {
      console.error(docker('logs', '--tail', '150', name))
    }
    catch {}

    throw error
  }
  finally {
    if (started) {
      docker('rm', '-f', name)
      docker('run', '--rm', '--user', '0:0', '-v', `${root}:/fixture`, '--entrypoint', '/bin/rm', image, '-rf', '/fixture/data', '/fixture/state')
    }

    await rm(root, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
