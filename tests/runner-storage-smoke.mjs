// Real controller/guest direct image durability, pressure and cold reopen path.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { storageProbeCommand } from './storage-probe-command.mjs'

export async function prepareLocalStorage({ api }) {
  const policy = { memoryCacheMiB: 8, reserveMiB: 64, reservePercent: 1 }
  const storage = { owner: randomUUID(), policy }
  let generation = 0

  async function applyPolicy(policy) {
    await api('/storage-policy', 'POST', { generation: generation++, policy })
  }

  await applyPolicy(policy)
  return { policy, storage, applyPolicy }
}

export async function inspectLocalDisk(api, runId) {
  const status = await (await api(`/disks/${runId}/storage-status`, 'POST', {})).json()
  assert.equal(status.mode, 'on-demand')
  assert.ok(status.localBytes > 0, 'The environment retains its local backing')
  assert.equal(status.grantId, undefined, 'Local disks expose no remote read grant')
  assert.equal(status.storage, 'local-ext4-v1')
  assert.equal(status.availability, 'available')
  return status
}

export async function storageSmoke({
  root,
  docker,
  name,
  api,
  until,
  reconnect,
  storageFixture,
}) {
  const runId = randomUUID()
  const workspace = `/data/runs/${runId}/workspace`
  await mkdir(path.join(root, 'data/runs', runId, 'workspace'), { recursive: true })
  const { policy, storage, applyPolicy } = storageFixture || await prepareLocalStorage({ api, root })
  const inbox = path.join(root, 'data/runs', runId, 'chat-input')
  await mkdir(inbox)
  const status = () => inspectLocalDisk(api, runId)

  async function start(first, { benchmark = false, soak = false } = {}) {
    const id = randomUUID()
    await writeFile(path.join(inbox, 'messages.json'), '[]')
    const plan = {
      id,
      runId,
      expires: null,
      sandbox: 'yolo',
      cwd: workspace,
      command: ['/usr/local/bin/node', '-e', storageProbeCommand({
        workspace,
        first,
        benchmark,
        soak,
      })],
      resources: { cpu: 1, memoryMiB: 512, diskMiB: 512 },
      storage: { ...storage, owner: runId },
      imports: [{ source: workspace, target: workspace }, { source: `/data/runs/${runId}/chat-input`, target: '/run/cairn-chat', readOnly: true }],
    }
    await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
    await api(`/runs/${id}`, 'POST')
    return id
  }

  async function logs(id) {
    try {
      return (await readFile(path.join(root, 'state', `${id}.log`), 'utf8')).split('\n').filter(Boolean).map(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString()).join('')
    }
    catch { return '' }
  }

  async function ready(id) {
    try {
      await until(async () => (await logs(id)).includes('storage.ready'))
    }
    catch (error) {
      console.error({
        runId,
        attempt: id,
        storage: await status(),
        guest: await logs(id),
      })
      throw error
    }
  }

  async function stop(id) {
    await api(`/runs/${id}`, 'DELETE')
    await until(async () => (await (await api('/health')).json()).activeRuns === 0)
  }

  async function finish(id) {
    await writeFile(path.join(inbox, 'messages.json'), '[{"text":"finish"}]')
    const exit = await (await api(`/runs/${id}/wait`, 'POST')).json()
    assert.equal(exit.StatusCode, 0, await logs(id))
    await until(async () => (await (await api('/health')).json()).activeRuns === 0)
    let sample
    try {
      await until(async () => {
        sample = await status()
        return sample.storage === 'local-ext4-v1' && sample.availability === 'available' && !sample.paused
      })
    }
    catch (error) {
      console.error({
        runId,
        attempt: id,
        storage: sample,
        guest: await logs(id),
      })
      throw error
    }
  }

  const first = await start(true)
  await ready(first)
  await status()
  docker('exec', name, 'test', '-f', `/runner-state/disks/${runId}/data.ext4`)
  docker('exec', name, 'test', '!', '-e', `/runner-state/disks/${runId}/lazy`)
  docker('exec', name, 'test', '!', '-e', '/dev/ublk-control')
  await finish(first)

  const soakSeconds = Number(process.env.CAIRN_STORAGE_SOAK_SECONDS || 0)
  if (soakSeconds > 0) {
    assert.ok(Number.isFinite(soakSeconds) && soakSeconds >= 60 && soakSeconds <= 3600)
    const attempt = await start(false, { soak: true })
    await ready(attempt)
    const vmIdentity = () => JSON.parse(docker('exec', name, 'cat', `/runner-state/${attempt}.vm.json`)).vmId
    const initialVm = vmIdentity()
    const started = performance.now()
    const resources = []
    while (performance.now() - started < soakSeconds * 1000) {
      await setTimeout(5000)
      const sample = await status()
      assert.equal(vmIdentity(), initialVm, 'Local maintenance must not replace the VM')
      assert.equal(sample.waitingFor, null)
      resources.push({ localBytes: sample.localBytes, size: sample.size })
    }

    const metrics = [...(await logs(attempt)).matchAll(/storage.soak (\{[^\n]+\})/g)].map(match => JSON.parse(match[1]))
    assert.ok(metrics.length > soakSeconds / 2)
    for (let index = 1; index < metrics.length; index++)
      assert.ok(metrics[index].writes > metrics[index - 1].writes, 'Durable guest writes stalled')
    assert.ok(metrics.at(-1).writes >= soakSeconds * 20)
    assert.ok(Math.max(...metrics.map(sample => sample.maximumMs)) < 1000)
    assert.ok(resources.every(sample => sample.localBytes > 0 && sample.size === 512 * 1048576))
    assert.ok(Math.max(...resources.map(sample => sample.localBytes)) - Math.min(...resources.map(sample => sample.localBytes)) < 8 * 1048576, 'Overwrites must not accumulate journal history')
    const evidence = {
      kind: 'storage-vm-soak',
      seconds: (performance.now() - started) / 1000,
      attempt,
      vmId: initialVm,
      metrics,
      resources,
      unintendedRestarts: 0,
      source: 'Real Firecracker/direct raw image; no remote disk source or WAN',
    }
    if (process.env.CAIRN_STORAGE_SOAK_EVIDENCE)
      await writeFile(process.env.CAIRN_STORAGE_SOAK_EVIDENCE, JSON.stringify(evidence, null, 2))
    process.stdout.write(`${JSON.stringify(evidence)}\n`)
    await finish(attempt)
  }

  // Reopening the same local disk verifies the first guest's synced bytes.
  const resumed = await start(false)
  await ready(resumed)
  await applyPolicy({ ...policy, reserveMiB: 16777216 })
  await until(async () => (await status()).waitingFor === 'disk-space')
  await setTimeout(2000)
  const ticks = (await logs(resumed)).split('storage.tick').length
  await setTimeout(2000)
  assert.equal((await logs(resumed)).split('storage.tick').length, ticks)
  await applyPolicy(policy)
  await until(async () => (await logs(resumed)).split('storage.tick').length > ticks)
  // A blocked local disk also remains cancellable while CPUs are paused.
  await applyPolicy({ ...policy, reserveMiB: 16777216 })
  await until(async () => (await status()).waitingFor === 'disk-space')
  const cancelledAt = Date.now()
  await stop(resumed)
  assert.ok(Date.now() - cancelledAt < 30000, 'Local pressure remains cancellable')
  await applyPolicy(policy)
  const recovered = await start(false)
  await ready(recovered)
  await finish(recovered)

  // A committed image marker must never permit a missing retained image to
  // become a fresh disk. Withhold only this stopped environment's own file.
  const withheld = JSON.parse(docker('exec', name, '/usr/local/bin/node', '-e', `
    const fs=require('node:fs');
    let directory='/runner-state/disks/${runId}';
    if(fs.existsSync(directory+'/environment'))
      directory='/runner-state/environments/'+fs.readFileSync(directory+'/environment','utf8').trim();
    const record=JSON.parse(fs.readFileSync(directory+'/image.json','utf8'));
    if(record.format!=='local-ext4-v1')throw Error('Missing committed image');
    const file=directory+'/data.ext4',backup=file+'.integrity-test';
    fs.renameSync(file,backup);console.log(JSON.stringify({file,backup}));
  `))
  let integrityRefused = false
  try {
    docker('restart', name)
    await reconnect()
    const broken = await start(false)
    const exit = await (await api(`/runs/${broken}/wait`, 'POST')).json()
    assert.notEqual(exit.StatusCode, 0, 'A missing retained image must refuse guest startup')
    assert.ok(!(await logs(broken)).includes('storage.ready'))
    integrityRefused = true
  }
  finally {
    docker('exec', name, '/usr/local/bin/node', '-e', `require('node:fs').renameSync(${JSON.stringify(withheld.backup)},${JSON.stringify(withheld.file)})`)
    docker('restart', name)
    await reconnect()
  }

  // The controller and guest caches restart; the host page cache is not flushed.
  // Every sample reads the same locally retained 32 MiB file and verifies its hash.
  for (let sample = 0; sample < 3; sample++) {
    await writeFile(path.join(inbox, 'messages.json'), '[]')
    docker('restart', name)
    await reconnect()
    const at = performance.now()
    const id = await start(false, { benchmark: true })
    await ready(id)
    const bootMs = performance.now() - at
    await writeFile(path.join(inbox, 'messages.json'), '[{"text":"measure"}]')
    const metrics = await until(async () => {
      const match = (await logs(id)).match(/storage.metrics (\{[^\n]+\})/)
      return match && JSON.parse(match[1])
    })
    assert.ok(Number.isFinite(metrics.mibPerSecond) && metrics.mibPerSecond > 0)
    await status()
    process.stdout.write(`${JSON.stringify({
      benchmark: 'local-conversation-disk',
      sample,
      bootMs,
      remoteEncodedBytes: 0,
      ...metrics,
      source: 'Same-node controller/VM restart, direct raw image, warm host page cache; no WAN',
    })}\n`)
    await finish(id)
  }

  await api(`/disks/${runId}/delete`, 'POST', { permanentlyDeleted: true })
  process.stdout.write(`${JSON.stringify({
    mode: 'local-image-controller',
    pressureResume: true,
    pressureCancellation: true,
    integrityRefused,
    sameNodeDurability: true,
    status: 'passed',
  })}\n`)
}
