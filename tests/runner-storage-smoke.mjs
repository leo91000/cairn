// Real controller/guest storage path. The loopback immutable origin deliberately
// replaces S3 here; encrypted S3 publication is covered by node_s3_test.py.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { createHash, randomUUID } from 'node:crypto'
import {
  mkdir,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

export async function prepareStorageOrigin({
  root,
  docker,
  name,
  api,
  maxDirtySeconds = 300,
}) {
  const origin = path.join(root, 'data/storage-fixture')
  await mkdir(origin)
  await writeFile(path.join(origin, 'server.mjs'), `
    import http from 'node:http'; import fs from 'node:fs';
    const root='/data/storage-fixture';
    http.createServer((req,res)=>{
      if(req.headers.authorization!=='Bearer fixture-storage-grant'){res.writeHead(403).end();return;}
      if(req.url==='/internal/node-restore/renew'){res.end('{}');return;}
      if(fs.existsSync(root+'/offline')){res.writeHead(503).end();return;}
      const hash=req.url.split('/').at(-1);
      if(!/^[a-f0-9]{64}$/.test(hash)||!fs.existsSync(root+'/'+hash)){res.writeHead(404).end();return;}
      const delay=Number(fs.existsSync(root+'/latency')?fs.readFileSync(root+'/latency','utf8'):0);
      setTimeout(()=>{const bytes=fs.readFileSync(root+'/'+hash);
        fs.appendFileSync(root+'/reads',hash+'\\n'); res.end(bytes);},delay);
    }).listen(4313,'127.0.0.1');
  `)
  docker('exec', '-d', name, '/usr/local/bin/node', '/data/storage-fixture/server.mjs')
  const policy = {
    cacheMiB: 8,
    reserveMiB: 64,
    reservePercent: 1,
    backupSeconds: 60,
    maxDirtySeconds,
  }
  const storage = { master: 'http://127.0.0.1:4313/', grant: 'fixture-storage-grant', policy }
  await api('/storage-policy', 'POST', policy)
  return { origin, policy, storage }
}

export async function storageSmoke({
  root,
  docker,
  name,
  api,
  until,
  storageFixture,
}) {
  const runId = randomUUID()
  const workspace = `/data/runs/${runId}/workspace`
  await mkdir(path.join(root, 'data/runs', runId, 'workspace'), { recursive: true })
  const { origin, policy, storage } = storageFixture || await prepareStorageOrigin({
    root,
    docker,
    name,
    api,
  })
  const inbox = path.join(root, 'data/runs', runId, 'chat-input')
  await mkdir(inbox)

  async function start(first, { benchmark = false, soak = false } = {}) {
    const id = randomUUID()
    const code = `
      const fs=require('node:fs'),assert=require('node:assert/strict'),cp=require('node:child_process');
      const file=${JSON.stringify(`${workspace}/saved`)};
      const unused=${JSON.stringify(`${workspace}/unused`)};
      const hash=bytes=>require('node:crypto').createHash('sha256').update(bytes).digest('hex');
      if(${first}) {fs.writeFileSync(file,'journal survives'); fs.writeFileSync(${JSON.stringify(`${workspace}/unused`)},require('node:crypto').randomBytes(32*1024*1024)); fs.writeFileSync(unused+'.sha256',hash(fs.readFileSync(unused))); cp.execFileSync('sync');}
      const started=performance.now();
      assert.equal(fs.readFileSync(file,'utf8'),'journal survives');
      const savedReadMs=performance.now()-started;
      console.log('storage.ready'); setInterval(()=>console.log('storage.tick'),1000);
      if(${soak}) {
        const fd=fs.openSync(file+'.load','w+'), data=Buffer.alloc(4096), read=Buffer.alloc(4096);
        let sequence=0, maximumMs=0;const latencies=[];
        setInterval(()=>{
          sequence++;data.fill(sequence%251);
          const offset=(sequence%128)*4096,at=performance.now();
          assert.equal(fs.writeSync(fd,data,0,data.length,offset),data.length);fs.fsyncSync(fd);
          assert.equal(fs.readSync(fd,read,0,read.length,offset),read.length);assert.deepEqual(read,data);
          const ms=performance.now()-at;maximumMs=Math.max(maximumMs,ms);latencies.push(ms);
          if(latencies.length>5000)latencies.shift();
        },10);
        setInterval(()=>{
          const sorted=[...latencies].sort((a,b)=>a-b);
          console.log('storage.soak '+JSON.stringify({writes:sequence,maximumMs,p99Ms:sorted[Math.floor(sorted.length*.99)]||0}));
        },1000);
      }
      if(${benchmark}) {
        const control=setInterval(()=>{
          if(!fs.readFileSync('/run/leo-chat/messages.json','utf8').includes('measure'))return;
          clearInterval(control);
          const fd=fs.openSync(unused,'r'), buffer=Buffer.alloc(4096), firstReadsMs=[];
          for(const offset of [0,8*1024*1024,16*1024*1024]) {
            const at=performance.now(); assert.equal(fs.readSync(fd,buffer,0,buffer.length,offset),4096);
            firstReadsMs.push(performance.now()-at);
          }
          fs.closeSync(fd);
          const at=performance.now(), bytes=fs.readFileSync(unused), elapsedMs=performance.now()-at;
          assert.equal(bytes.length,32*1024*1024);
          assert.equal(hash(bytes),fs.readFileSync(unused+'.sha256','utf8'));
          console.log('storage.metrics '+JSON.stringify({savedReadMs,firstReadsMs,sequentialMs:elapsedMs,mibPerSecond:32/(elapsedMs/1000)}));
        },50);
      }
    `
    const plan = {
      id,
      runId,
      expires: null,
      sandbox: 'yolo',
      cwd: workspace,
      command: ['/usr/local/bin/node', '-e', code],
      resources: { cpu: 1, memoryMiB: 512, diskMiB: 512 },
      storage,
      imports: [{ source: workspace, target: workspace }, ...(benchmark ? [{ source: `/data/runs/${runId}/chat-input`, target: '/run/leo-chat', readOnly: true }] : [])],
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

  const status = async () => (await (await api(`/disks/${runId}/storage-status`, 'POST', {})).json())

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

  async function publish(id) {
    const point = await (await api(`/runs/${id}/snapshot`, 'POST', {})).json()
    const hashes = new Set()
    for (const block of point.manifest.blocks) {
      if (!block.hash || hashes.has(block.hash))
        continue
      hashes.add(block.hash)
      const bytes = Buffer.from(await (await api(`/snapshots/${point.id}/${block.hash}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(bytes).digest('hex'), block.hash)
      await writeFile(path.join(origin, block.hash), bytes)
    }

    const backupId = randomUUID()
    await api(`/disks/${runId}/published`, 'POST', { generation: point.manifest.generation, grantId: point.grantId, backupId })
    await api(`/snapshots/${point.id}/discard`, 'DELETE')
    return { point, backupId, hashes }
  }

  const first = await start(true)
  await ready(first)
  const { point, backupId, hashes } = await publish(first)
  assert.equal((await status()).mode, 'on-demand')
  docker('exec', name, 'test', '!', '-e', `/runner-state/disks/${runId}/data.ext4`)
  await stop(first)

  // Opt-in sustained VM qualification, separate from the short CI smoke path.
  const soakSeconds = Number(process.env.LEO_STORAGE_SOAK_SECONDS || 0)
  if (soakSeconds > 0) {
    assert.ok(Number.isFinite(soakSeconds) && soakSeconds >= 60 && soakSeconds <= 3600)
    const attempt = await start(false, { soak: true })
    await ready(attempt)
    const vmIdentity = () => JSON.parse(docker('exec', name, 'cat', `/runner-state/${attempt}.vm.json`)).vmId
    const initialVm = vmIdentity()
    const started = performance.now()
    const publications = []
    const resources = []
    while (performance.now() - started < soakSeconds * 1000) {
      await setTimeout(5000)
      const at = performance.now()
      const saved = await publish(attempt)
      publications.push({
        generation: saved.point.manifest.generation,
        totalMs: performance.now() - at,
        pauseMs: saved.point.manifest.pauseMs,
        indexMs: saved.point.manifest.indexMs,
      })
      assert.equal(vmIdentity(), initialVm, 'checkpoint must not replace the VM')
      const sample = await status()
      assert.equal(sample.waitingFor, null)
      assert.equal(sample.published.backupId, saved.backupId)
      resources.push(sample.performance)
    }

    const output = await logs(attempt)
    const metrics = [...output.matchAll(/storage.soak (\{[^\n]+\})/g)].map(match => JSON.parse(match[1]))
    assert.ok(metrics.length > soakSeconds / 2, 'guest must keep progressing under repeated backups')
    for (let index = 1; index < metrics.length; index++)
      assert.ok(metrics[index].writes > metrics[index - 1].writes, 'durable guest writes stalled')
    assert.ok(publications.length >= 6)
    assert.ok(metrics.at(-1).writes >= soakSeconds * 20, 'sustained durable write load')
    assert.ok(Math.max(...metrics.map(sample => sample.maximumMs)) < 1000, 'guest disk stall exceeded the old controller timeout')
    assert.ok(resources.every(sample => sample.write.errors === 0 && sample.read.errors === 0))
    const evidence = {
      kind: 'storage-vm-soak',
      seconds: (performance.now() - started) / 1000,
      attempt,
      vmId: initialVm,
      publications,
      metrics,
      resources,
      unintendedRestarts: 0,
      source: 'real Firecracker/FUSE journal, loopback immutable origin; not S3/WAN',
    }
    if (process.env.LEO_STORAGE_SOAK_EVIDENCE)
      await writeFile(process.env.LEO_STORAGE_SOAK_EVIDENCE, JSON.stringify(evidence, null, 2))
    process.stdout.write(`${JSON.stringify(evidence)}\n`)
    await stop(attempt)
    // Verify durable guest contents on a new VM after all acknowledged backups.
    const recovered = await start(false)
    await ready(recovered)
    await stop(recovered)
  }

  // Restore into a separate empty directory, retaining the source for comparison.
  docker('exec', name, 'mv', `/runner-state/disks/${runId}`, `/runner-state/disks/source-${runId}`)
  await writeFile(path.join(origin, 'offline'), '')
  const restoredAt = Date.now()
  await api(`/disks/${runId}/restore`, 'POST', { ...storage, manifest: point.manifest, backupId })
  const metadataRestoreMs = Date.now() - restoredAt
  const blocked = await start(false)
  await until(async () => (await status()).waitingFor === 'storage-unavailable')
  const cancelledAt = Date.now()
  await stop(blocked)
  assert.ok(Date.now() - cancelledAt < 30000, 'blocked remote read remains cancellable')
  await rm(path.join(origin, 'offline'))
  await writeFile(path.join(origin, 'reads'), '')
  const resumedAt = Date.now()
  const resumed = await start(false)
  await ready(resumed)
  const resumedMs = Date.now() - resumedAt
  const fetched = new Set((await readFile(path.join(origin, 'reads'), 'utf8')).trim().split('\n').filter(Boolean))
  assert.ok(fetched.size < hashes.size, 'unused data stays remote during boot')
  // Verify a real CPU pause and automatic resume when the reserve changes.
  await api('/storage-policy', 'POST', { ...policy, reserveMiB: 16777216 })
  await until(async () => (await status()).waitingFor === 'disk-space')
  await setTimeout(2000)
  const ticks = (await logs(resumed)).split('storage.tick').length
  await setTimeout(2000)
  assert.equal((await logs(resumed)).split('storage.tick').length, ticks)
  // Emergency publication while paused must not strand CPUs after pressure clears.
  await publish(resumed)
  await api('/storage-policy', 'POST', policy)
  await until(async () => (await logs(resumed)).split('storage.tick').length > ticks)
  await stop(resumed)
  // Identical snapshot, guest command and read workload; each sample gets a new
  // VM and empty disk directory. The host page cache is intentionally not flushed.
  const sizes = new Map(point.manifest.blocks.filter(block => block.hash).map(block => [block.hash, block.size]))

  async function downloaded() {
    const reads = (await readFile(path.join(origin, 'reads'), 'utf8')).trim().split('\n').filter(Boolean)
    return reads.reduce((bytes, hash) => bytes + sizes.get(hash), 0)
  }

  const modes = [
    { mode: 'demand-http', latencyMs: 0 },
    { mode: 'demand-http-delayed', latencyMs: 50 },
  ]
  for (let sample = 0; sample < 3; sample++) {
    // Rotate order to reduce systematic warm-host effects.
    for (let index = 0; index < modes.length; index++) {
      const mode = modes[(index + sample) % modes.length]
      docker('exec', name, 'mv', `/runner-state/disks/${runId}`, `/runner-state/disks/previous-${randomUUID()}`)
      await writeFile(path.join(inbox, 'messages.json'), '[]')
      await writeFile(path.join(origin, 'latency'), String(mode.latencyMs))
      await writeFile(path.join(origin, 'reads'), '')
      const at = performance.now()
      await api(`/disks/${runId}/restore`, 'POST', { ...storage, manifest: point.manifest, backupId })
      const restoreMs = performance.now() - at
      const id = await start(false, { benchmark: true })
      await ready(id)
      const availableMs = performance.now() - at
      const bytesAtReady = await downloaded()
      assert.equal((await status()).mode, 'on-demand')
      assert.ok(bytesAtReady < [...sizes.values()].reduce((a, b) => a + b, 0), 'ready before full download')
      await writeFile(path.join(inbox, 'messages.json'), '[{"text":"measure"}]')
      const metrics = await until(async () => {
        const match = (await logs(id)).match(/storage.metrics (\{[^\n]+\})/)
        return match && JSON.parse(match[1])
      })
      const bytesAfterReads = await downloaded()
      assert.ok(Number.isFinite(metrics.mibPerSecond) && metrics.mibPerSecond > 0)
      process.stdout.write(`${JSON.stringify({
        benchmark: 'conversation-disk',
        sample,
        ...mode,
        restoreMs,
        availableMs,
        bootMs: availableMs - restoreMs,
        bytesAtReady,
        bytesAfterReads,
        ...metrics,
        source: 'loopback HTTP with optional per-request delay; not S3/WAN',
      })}\n`)
      await stop(id)
    }
  }

  process.stdout.write(`${JSON.stringify({
    mode: 'on-demand-controller',
    metadataRestoreMs,
    resumedMs,
    fetchedBlocks: fetched.size,
    remoteBlocks: hashes.size,
    cancellableOutage: true,
    pressureResume: true,
    status: 'passed',
    source: 'loopback immutable origin, not S3 benchmark',
  })}\n`)
}
