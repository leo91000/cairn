// Real native Codex/ublk, four simultaneous guests, publication and crash recovery.
// Compare with CAIRN_VM_SNAPSHOTS=false. Requires a host with KVM and ublk_drv.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync, spawnSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { readFileSync } from 'node:fs'
import {
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  writeFile,
} from 'node:fs/promises'
import { createServer } from 'node:net'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { blockDigest } from './block-digest.mjs'
import { prepareStorageOrigin } from './runner-storage-smoke.mjs'

async function main() {
  const image = process.argv[2]
  assert.ok(image, 'Provide the runner image')
  const snapshots = process.env.CAIRN_VM_SNAPSHOTS !== 'false'
  const poolSize = Number(process.env.CAIRN_READY_VM_POOL_SIZE ?? 1)
  const memoryGiB = Number(process.env.CAIRN_SNAPSHOT_TEST_MEMORY_GIB ?? 16)
  const cpuQuota = Number(process.env.CAIRN_SNAPSHOT_TEST_CPU ?? 4)
  const slots = Number(process.env.CAIRN_SNAPSHOT_TEST_SLOTS ?? 4)
  assert.ok(Number.isInteger(poolSize) && poolSize >= 1 && poolSize <= 4)
  assert.ok([memoryGiB, cpuQuota, slots].every(value => Number.isInteger(value) && value > 0))
  const docker = (...args) => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 180000 }).trim()
  const imageId = docker('image', 'inspect', '--format', '{{.Id}}', image)
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || '/var/tmp', 'cairn-snapshots-'))
  const name = `cairn-snapshots-${randomUUID().slice(0, 8)}`
  const peer = `${name}-peer`
  const network = `${name}-public`
  const endpoint = 'http://203.0.113.3:8080'
  const devices = readFileSync('/proc/devices', 'utf8')
  const charMajor = Number(devices.match(/(?:^|\n)\s*(\d+) ublk-char(?:\n|$)/)?.[1])
  const blockMajor = Number(devices.split('Block devices:')[1]?.match(/(?:^|\n)\s*(\d+) (?:ublk|blkext)(?:\n|$)/)?.[1])
  assert.ok(charMajor > 0 && blockMajor > 0, 'Load ublk_drv before running this native test')
  const brokers = []
  const plans = []
  let url

  async function until(operation, timeout = 120000) {
    const deadline = Date.now() + timeout
    while (Date.now() < deadline) {
      const result = await operation()
      if (result)
        return result
      await setTimeout(50)
    }

    throw new Error('Snapshot qualification timed out')
  }

  async function api(route, method = 'GET', body) {
    const response = await fetch(url + route, {
      method,
      headers: { 'authorization': 'Bearer fixture-runner-token', 'content-type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(120000),
    })
    assert.ok(response.ok, `${method} ${route}: ${response.status} ${response.ok ? '' : await response.text()}`)
    return response
  }

  async function reconnect() {
    url = `http://${docker('port', name, '4311/tcp')}`
    await until(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false))
  }

  function records() {
    return JSON.parse(docker('exec', peer, 'node', '-e', 'fetch("http://127.0.0.1:8080/records").then(r=>r.json()).then(v=>console.log(JSON.stringify(v)))'))
  }

  async function guestLogs(id) {
    const raw = await readFile(path.join(root, 'state', `${id}.log`), 'utf8')
    return raw.split('\n').filter(Boolean).map(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString()).join('')
  }

  async function finish(plan) {
    const result = await (await api(`/runs/${plan.id}/wait`, 'POST')).json()
    assert.equal(result.StatusCode, 0, await guestLogs(plan.id))
    assert.match(await readFile(path.join(root, 'data/runs', plan.runId, 'output/result.md'), 'utf8'), /SNAPSHOT_NATIVE_OK/)
    assert.ok(plan.credentials() > 0, 'The clone uses its own managed account broker')
    const logs = await guestLogs(plan.id)
    const thread = logs.match(/"thread_id":"([^"]+)"/)?.[1]
    assert.ok(thread, 'Native Codex reports the persisted thread')
    return thread
  }

  try {
    for (const directory of ['data/runner-plans', 'state'])
      await mkdir(path.join(root, directory), { recursive: true })
    await writeFile(path.join(root, 'data/runner-secret'), 'fixture-runner-token')
    // Match the policy used by the first grants, so the anonymous template is reusable.
    await writeFile(path.join(root, 'state/storage-policy.json'), JSON.stringify({
      cacheMiB: 8,
      reserveMiB: 64,
      reservePercent: 1,
      backupSeconds: 60,
      maxDirtySeconds: 300,
    }))
    await copyFile(new URL('./fixtures/snapshot-model.mjs', import.meta.url), path.join(root, 'peer.mjs'))
    await copyFile(new URL('./fixtures/snapshot-memory.mjs', import.meta.url), path.join(root, 'data/memory.mjs'))
    docker('network', 'create', '--internal', '--subnet', '203.0.113.0/29', network)
    docker('run', '-d', '--name', peer, '-v', `${root}/peer.mjs:/peer.mjs:ro`, '--entrypoint', 'node', imageId, '/peer.mjs')
    docker('network', 'connect', '--ip', '203.0.113.3', network, peer)
    // SYS_PTRACE is observer-only: it permits smaps inspection of other UIDs.
    const capabilities = ['SYS_RESOURCE', 'SYS_PTRACE', 'SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE']
    docker('run', '-d', '--name', name, '--user', '0:0', '--no-healthcheck', '--read-only', '--cap-drop', 'ALL', ...capabilities.flatMap(value => ['--cap-add', value]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', '--device', '/dev/kvm', '--device', '/dev/fuse', '--device', '/dev/ublk-control', '--device-cgroup-rule', `c ${charMajor}:* rwm`, '--device-cgroup-rule', `b ${blockMajor}:* rwm`, '--device', '/dev/net/tun', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', `${memoryGiB}g`, '--cpus', `${cpuQuota}`, '-e', `CONCURRENCY=${slots}`, '-e', `APP_RUNTIME_ID=snapshots-test-${imageId.slice(7)}`, '-e', 'CAIRN_READY_VM_POOL=true', '-e', `CAIRN_READY_VM_POOL_SIZE=${poolSize}`, '-e', 'CAIRN_BLOCK_TRANSPORT=ublk', '-e', 'CAIRN_DISK_LAYOUT=paired-ext4-v1', '-e', `CAIRN_VM_SNAPSHOTS=${snapshots}`, '-e', 'RUST_LOG=warn,cairn_performance=info', '--entrypoint', '/usr/local/bin/cairn', imageId, 'runner-broker')
    docker('network', 'connect', '--ip', '203.0.113.2', network, name)
    await reconnect()
    const fixture = await prepareStorageOrigin({
      root,
      docker,
      name,
      api,
    })
    await until(async () => (await (await api('/health')).json()).pool.ready === poolSize, 300000)

    for (let index = 0; index < 4; index++) {
      const runId = randomUUID()
      const id = randomUUID()
      const marker = `snapshot-${runId}`
      const runRoot = `/data/runs/${runId}`
      const source = path.join(root, 'data/runs', runId)
      for (const directory of ['workspace', 'home/.codex', 'output', 'chat-input'])
        await mkdir(path.join(source, directory), { recursive: true })
      await writeFile(path.join(source, 'home/.codex/config.toml'), 'cli_auth_credentials_store = "file"\n')
      await writeFile(path.join(source, 'home/.codex/cairn-managed-auth'), '1')
      await writeFile(path.join(source, 'chat-input/messages.json'), '[]')
      await copyFile(new URL('./fixtures/nested-kvm.c', import.meta.url), path.join(source, 'workspace/nested-kvm.c'))
      let credentials = 0
      const claims = Buffer.from(JSON.stringify({ 'sub': marker, 'exp': Math.floor(Date.now() / 1000) + 3600, 'https://api.openai.com/auth': { chatgpt_account_id: marker, chatgpt_plan_type: 'plus' } })).toString('base64url')
      const broker = createServer(client => client.once('data', () => {
        credentials++
        client.end(`${JSON.stringify({ accessToken: `eyJhbGciOiJub25lIn0.${claims}.synthetic`, chatgptAccountId: marker, chatgptPlanType: 'plus' })}\n`)
      }))
      brokers.push(broker)
      await new Promise(resolve => broker.listen(path.join(source, 'home/.codex/cairn-auth.sock'), resolve))
      const provider = {
        name: 'Fixture',
        base_url: endpoint,
        wire_api: 'responses',
        requires_openai_auth: false,
      }
      const chat = {
        runId,
        attemptId: id,
        provider: 'codex',
        execution: { messageId: randomUUID(), text: `Execute the durable marker test ${marker}. HOLD_SNAPSHOT`, recovery: false },
        instructions: 'Execute the requested tool and report its result.',
        inputDirectory: '/run/cairn-chat',
        output: `${runRoot}/output/result.md`,
        cwd: `${runRoot}/workspace`,
        model: 'gpt-5.4',
        reasoning: 'low',
        sandbox: 'yolo',
        writableRoots: [`${runRoot}/workspace`],
        args: ['-c', 'model_provider="fixture"', '-c', `model_providers.fixture={name="Fixture",base_url="${endpoint}",wire_api="responses",requires_openai_auth=false}`],
        codexConfig: { mcp_servers: {}, model_provider: 'fixture', model_providers: { fixture: provider } },
      }
      plans.push({
        id,
        runId,
        marker,
        credentials: () => credentials,
        plan: {
          id,
          runId,
          expires: Date.now() + 120000,
          sandbox: 'yolo',
          cwd: chat.cwd,
          resources: { cpu: cpuQuota, memoryMiB: memoryGiB * 1024 - 512, diskMiB: 32768 },
          storage: fixture.storage,
          chat,
          imports: [{ source: `${runRoot}/workspace`, target: chat.cwd }, { source: `${runRoot}/home`, target: '/home/node' }, { source: `${runRoot}/output`, target: `${runRoot}/output` }, { source: `${runRoot}/chat-input`, target: '/run/cairn-chat', readOnly: true }],
        },
      })
    }

    for (const entry of plans)
      await writeFile(path.join(root, 'data/runner-plans', `${entry.id}.json`), JSON.stringify(entry.plan))
    const poolBefore = (await (await api('/health')).json()).pool
    const poolMemoryBefore = JSON.parse(docker('exec', name, 'node', '/data/memory.mjs'))
    const started = Date.now()
    await Promise.all(plans.map(entry => api(`/runs/${entry.id}`, 'POST')))
    await until(() => {
      const requests = records()
      assert.ok(!requests.some(request => request.failed), 'Native tool failed; inspect the private guest logs')
      return plans.every(entry => requests.some(request => request.marker === entry.marker && request.executed))
    })
    const memory = JSON.parse(docker('exec', name, 'node', '/data/memory.mjs'))
    await writeFile(path.join(root, 'memory.json'), JSON.stringify(memory, null, 2))
    const activeIds = new Set(plans.map(entry => JSON.parse(docker('exec', name, 'cat', `/runner-state/${entry.id}.vm.json`)).vmId))
    const activeVmms = memory.vmms.filter(vm => activeIds.has(vm.vmId))
    assert.equal(activeVmms.length, 4, 'Four owned conversations are active during measurement')
    if (poolSize === 4) {
      const preparedIds = new Set(poolMemoryBefore.vmms.map(vm => vm.vmId))
      assert.ok([...activeIds].every(id => preparedIds.has(id)), 'All four arrivals claim their already initialized VM')
    }

    assert.match(memory.events, /^oom_kill 0$/m)
    const inodes = activeVmms.map(vm => vm.mappings[0]?.trim().split(/\s+/)[4])
    if (snapshots) {
      assert.ok(inodes.every(Boolean), 'All clones map snapshot memory')
      assert.equal(new Set(inodes).size, 1, 'All clones share one immutable memory inode')
      assert.ok(activeVmms.every(vm => vm.mappings.every(mapping => / rw-p /.test(mapping))), 'Guest memory uses private mappings')
    }
    else {
      assert.ok(inodes.every(inode => !inode), 'Control guests use the ordinary boot path')
    }

    const threads = await Promise.all(plans.map(finish))
    assert.equal(new Set(threads).size, 4, 'Each clone has an independent native thread')
    const vmIds = plans.map(entry => JSON.parse(docker('exec', name, 'cat', `/runner-state/${entry.id}.vm.json`)).vmId)
    assert.equal(new Set(vmIds).size, 4, 'Each conversation has an independent VM')
    const requests = records()
    const preModelMs = plans.map(entry => requests.find(request => request.marker === entry.marker).at - started)
    const published = []
    for (const entry of plans) {
      await api(`/runs/${entry.id}`, 'DELETE')
      const saved = await (await api(`/disks/${entry.runId}/snapshot`, 'POST', {})).json()
      assert.equal(saved.manifest.version, 2)
      const hashes = new Set()
      for (const block of saved.manifest.blocks) {
        if (!block.hash || hashes.has(block.hash))
          continue
        hashes.add(block.hash)
        const bytes = Buffer.from(await (await api(`/snapshots/${saved.id}/${block.hash}`)).arrayBuffer())
        assert.equal(blockDigest(bytes, block.hash), block.hash)
        await writeFile(path.join(fixture.origin, block.hash), bytes)
      }

      const backupId = randomUUID()
      await api(`/disks/${entry.runId}/published`, 'POST', { generation: saved.manifest.generation, grantId: saved.grantId, backupId })
      await api(`/snapshots/${saved.id}/discard`, 'DELETE')
      published.push({ manifest: saved.manifest, backupId, verifiedBlocks: hashes.size })
    }

    // Abrupt controller loss must not rely on the template or a retained VM.
    docker('kill', '--signal', 'KILL', name)
    docker('start', name)
    await reconnect()
    docker('exec', '-d', name, 'node', '/data/storage-fixture/server.mjs')
    for (const [index, entry] of plans.entries()) {
      const saved = published[index]
      await api(`/disks/${entry.runId}/restore`, 'POST', { ...fixture.storage, manifest: saved.manifest, backupId: saved.backupId })
      const id = randomUUID()
      const credentialsBefore = entry.credentials()
      const plan = {
        ...entry.plan,
        id,
        expires: Date.now() + 120000,
        chat: {
          ...entry.plan.chat,
          attemptId: id,
          sessionId: threads[index],
          execution: { messageId: randomUUID(), text: `VERIFY_SNAPSHOT ${entry.marker}`, recovery: false },
        },
      }
      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      await api(`/runs/${id}`, 'POST')
      assert.equal(await finish({ ...entry, id }), threads[index], 'The published native thread survives controller SIGKILL')
      assert.ok(entry.credentials() > credentialsBefore, 'Access is renewed after crash recovery')
      assert.ok(records().some(request => request.marker === entry.marker && request.verify && request.executed), 'Both system and workspace markers survive publication and crash')
      await api(`/runs/${id}`, 'DELETE')
    }

    process.stdout.write(`${JSON.stringify({
      imageId,
      snapshots,
      poolSize,
      poolBefore,
      poolMemoryBefore,
      activeVmms,
      root,
      preModelMs,
      memory,
      publications: published.map(saved => ({ generation: saved.manifest.generation, verifiedBlocks: saved.verifiedBlocks })),
      crashRestores: plans.length,
      scope: 'Real runner, Firecracker, ublk Async and native Codex; external synthetic account/model and local immutable HTTP origin, not production S3/WAN.',
    }, null, 2)}\n`)
  }
  finally {
    for (const broker of brokers)
      broker.close()
    const logs = spawnSync('docker', ['--context', 'default', 'logs', name], { encoding: 'utf8', timeout: 10000, maxBuffer: 32 * 1024 * 1024 })
    await writeFile(path.join(root, 'controller.log'), `${logs.stdout || ''}\n${logs.stderr || ''}`)
    for (const container of [name, peer]) {
      try {
        docker('rm', '-f', container)
      }
      catch {}
    }

    try {
      docker('network', 'rm', network)
    }
    catch {}

    console.error(JSON.stringify({ fixtureRoot: root }))
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
