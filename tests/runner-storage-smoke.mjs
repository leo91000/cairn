// Real controller/guest storage path. The loopback immutable origin deliberately
// replaces S3 here; encrypted S3 publication is covered by node_s3_test.py.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

export async function storageSmoke({ root, docker, name, api, until }) {
  const runId = randomUUID()
  const workspace = `/data/runs/${runId}/workspace`
  await mkdir(path.join(root, 'data/runs', runId, 'workspace'), { recursive: true })
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
      const bytes=fs.readFileSync(root+'/'+hash);
      fs.appendFileSync(root+'/reads',hash+'\\n'); res.end(bytes);
    }).listen(4313,'127.0.0.1');
  `)
  docker('exec', '-d', name, '/usr/local/bin/node', '/data/storage-fixture/server.mjs')
  const policy = { enabled: true, cacheMiB: 8, reserveMiB: 64, reservePercent: 1, backupSeconds: 60, maxDirtySeconds: 300, automaticArchiving: false }
  const storage = { master: 'http://127.0.0.1:4313/', grant: 'fixture-storage-grant', policy }
  await api('/storage-policy', 'POST', policy)
  async function start(first) {
    const id = randomUUID()
    const code = `
      const fs=require('node:fs'),assert=require('node:assert/strict'),cp=require('node:child_process');
      const file=${JSON.stringify(`${workspace}/saved`)};
      if(${first}) {fs.writeFileSync(file,'journal survives'); fs.writeFileSync(${JSON.stringify(`${workspace}/unused`)},require('node:crypto').randomBytes(32*1024*1024)); cp.execFileSync('sync');}
      assert.equal(fs.readFileSync(file,'utf8'),'journal survives');
      console.log('storage.ready'); setInterval(()=>console.log('storage.tick'),1000);
    `
    const plan = { id, runId, expires: null, sandbox: 'yolo', cwd: workspace, command: ['/usr/local/bin/node', '-e', code], resources: { cpu: 1, memoryMiB: 512, diskMiB: 512 }, storage, imports: [{ source: workspace, target: workspace }] }
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
  const ready = id => until(async () => (await logs(id)).includes('storage.ready'))
  const status = async () => (await (await api(`/disks/${runId}/storage-status`, 'POST', {})).json())
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
  // Restore into a separate empty directory, retaining the source for comparison.
  docker('exec', name, 'mv', `/runner-state/disks/${runId}`, `/runner-state/disks/source-${runId}`)
  await writeFile(path.join(origin, 'offline'), '')
  const restoredAt = Date.now()
  await api(`/disks/${runId}/restore`, 'POST', { ...storage, onDemand: true, manifest: point.manifest, backupId })
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
  process.stdout.write(`${JSON.stringify({ mode: 'on-demand-controller', metadataRestoreMs, resumedMs, fetchedBlocks: fetched.size, remoteBlocks: hashes.size, cancellableOutage: true, pressureResume: true, status: 'passed', source: 'loopback immutable origin, not S3 benchmark' })}\n`)
}
