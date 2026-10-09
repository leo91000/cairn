import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { prepareStorageOrigin } from './runner-storage-smoke.mjs'

// Physical memory counters complement the balloon's acknowledgement. A shared
// memfd can retain all pages even when the guest reports successful inflation.
export async function memorySmoke({
  root,
  docker,
  name,
  api,
  until,
  storageFixture,
}) {
  const { storage } = storageFixture || await prepareStorageOrigin({
    root,
    docker,
    name,
    api,
  })
  const id = randomUUID()
  const runId = randomUUID()
  const workspace = `/data/runs/${runId}/workspace`
  await mkdir(path.join(root, 'data/runs', runId, 'workspace'), { recursive: true })
  const { budget } = await (await api('/health')).json()
  const memoryMiB = budget.limits.memoryMiB - 512
  assert.ok(memoryMiB >= 2048)
  const code = [
    'const sentinel = Buffer.alloc(1024 * 1024, 42)',
    'let buffer = Buffer.alloc(512 * 1024 * 1024, 37)',
    'console.log("ram.allocated")',
    'setTimeout(() => { buffer = null; global.gc(); console.log("ram.freed") }, 5000)',
    'setInterval(() => { if (!sentinel.every(byte => byte === 42)) throw Error("Live guest data changed"); console.log("ram.alive") }, 1000)',
  ].join('\n')
  const plan = {
    id,
    runId,
    expires: null,
    sandbox: 'yolo',
    cwd: workspace,
    command: ['/usr/local/bin/node', '--expose-gc', '-e', code],
    resources: { cpu: 1, memoryMiB: 512, diskMiB: 32768 },
    storage,
    imports: [{ source: workspace, target: workspace }],
  }
  await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))

  async function output() {
    try {
      const lines = (await readFile(path.join(root, 'state', `${id}.log`), 'utf8')).split('\n').filter(Boolean)
      return lines.map(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString()).join('')
    }
    catch { return '' }
  }

  function sharedBytes() {
    const stat = docker('exec', name, 'cat', '/sys/fs/cgroup/memory.stat')
    const row = stat.split('\n').find(line => line.startsWith('shmem '))
    assert.ok(row)
    return Number(row.split(' ')[1])
  }

  let started = false
  try {
    await api(`/runs/${id}`, 'POST')
    started = true
    await until(async () => (await output()).includes('ram.allocated'))
    // cgroup counters are periodically batched; allow their update before measuring.
    await setTimeout(2000)
    const allocatedBytes = sharedBytes()
    const { vmId } = JSON.parse(docker('exec', name, 'cat', `/runner-state/${id}.vm.json`))
    const socket = `/runner-state/jails/firecracker/${vmId}/root/api.sock`
    const config = JSON.parse(docker('exec', name, 'cat', `/runner-state/jails/firecracker/${vmId}/root/config.json`))
    assert.equal(config['machine-config'].mem_size_mib, memoryMiB)
    assert.equal(config.drives[1].socket, 'disk.sock')

    function balloon(method, endpoint, body) {
      const request = JSON.stringify({ socketPath: socket, path: endpoint, method })
      const payload = body ? JSON.stringify(body) : ''
      const script = `
        const http = require('node:http');
        const request = ${request};
        const body = ${JSON.stringify(payload)};
        request.headers = {'content-type': 'application/json', 'content-length': Buffer.byteLength(body)};
        const call = http.request(request, response => {
          let data = '';
          response.on('data', chunk => data += chunk);
          response.on('end', () => console.log(JSON.stringify({status: response.statusCode, data})));
        });
        call.on('error', error => {console.error(error.message); process.exitCode = 1});
        call.end(body);
      `
      return JSON.parse(docker('exec', name, '/usr/local/bin/node', '-e', script))
    }

    await until(async () => (await output()).includes('ram.freed'))
    await setTimeout(3000)
    const amountMiB = memoryMiB - 1024
    assert.equal(balloon('PATCH', '/balloon', { amount_mib: amountMiB }).status, 204)
    const samples = []
    for (let index = 0; index < 5; index++) {
      await setTimeout(3000)
      samples.push(sharedBytes())
    }

    const response = balloon('GET', '/balloon/statistics')
    assert.equal(response.status, 200)
    const statistics = JSON.parse(response.data)
    assert.ok(statistics.actual_mib >= amountMiB - 128)
    const reclaimedBytes = allocatedBytes - samples.at(-1)
    assert.ok(reclaimedBytes >= 384 * 1024 * 1024, `Shared guest RAM stayed allocated: ${reclaimedBytes} bytes reclaimed`)
    const lines = await output()
    assert.ok((lines.match(/ram.alive/g) || []).length >= 15, 'Live guest memory must survive reclamation')
    assert.equal((await (await api('/health')).json()).activeRuns, 1)
    const evidence = {
      kind: 'shared-guest-memory-reclamation',
      memoryMiB,
      allocatedBytes,
      samples,
      reclaimedBytes,
      statistics,
    }
    if (process.env.CAIRN_MEMORY_EVIDENCE)
      await writeFile(process.env.CAIRN_MEMORY_EVIDENCE, JSON.stringify(evidence, null, 2))
    process.stdout.write(`${JSON.stringify(evidence)}\n`)
  }
  finally {
    if (started) {
      await api(`/runs/${id}`, 'DELETE')
      await until(async () => (await (await api('/health')).json()).activeRuns === 0)
    }
  }
}
