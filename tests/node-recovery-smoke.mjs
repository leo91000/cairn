// Real KVM evidence: active coherent capture and cold restore on another controller.
// Run with an image built from this branch; never point at a live controller's data.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync, spawn } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { copyFile, link, mkdir, mkdtemp, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as delay } from 'node:timers/promises'

async function main() {
  const assets = path.resolve(process.argv[2] || '')
  const binary = path.resolve(process.argv[3] || 'target/debug/leo')
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || path.dirname(assets), 'leo-node-kvm-'))
  const children = []
  const headers = { 'authorization': 'Bearer fixture-node-controller', 'content-type': 'application/json' }
  const run = randomUUID()
  const data = path.join(root, 'data')
  const workspace = path.join(data, 'runs', run, 'workspace')
  const home = path.join(data, 'runs', run, 'home')
  const blocks = new Map()
  let server
  async function request(port, route, method = 'GET', body) {
    const response = await fetch(`http://127.0.0.1:${port}${route}`, { method, headers, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(300000) })
    assert.ok(response.ok, `${method} ${route}: ${response.status} ${response.ok ? '' : await response.text()}`)
    return response
  }
  async function until(fn) {
    const deadline = Date.now() + 120000
    while (Date.now() < deadline) {
      if (await fn())
        return
      await delay(250)
    }
    throw new Error('KVM condition timed out')
  }
  async function controller(name, port) {
    const state = path.join(root, name)
    const image = path.join(state, 'images', 'nodes-integration')
    await mkdir(image, { recursive: true })
    await link(path.join(assets, 'root.ext4'), path.join(image, 'root.ext4'))
    await link(path.join(assets, 'vmlinux'), path.join(image, 'vmlinux'))
    const child = spawn('sudo', ['-n', 'unshare', '--mount', '--pid', '--fork', '--mount-proc', '--kill-child', 'sh', '-c', 'mount --bind "$1" /tmp; shift; exec "$@"', 'leo-kvm-fixture', state, 'env', `DATA_DIR=${data}`, 'RUNNER_STATE_DIR=/tmp', `RUNNER_BIND=127.0.0.1:${port}`, 'APP_RUNTIME_ID=nodes-integration', 'CONCURRENCY=1', binary, 'runner-broker'], { detached: true, stdio: ['ignore', 'pipe', 'pipe'] })
    children.push(child)
    let errors = ''
    child.stderr.on('data', b => errors += b.toString())
    child.stdout.resume()
    await until(async () => {
      assert.equal(child.exitCode, null, errors)
      try {
        return (await fetch(`http://127.0.0.1:${port}/health`)).ok
      }
      catch { return false }
    })
    return { state, child }
  }
  async function stopController(child) {
  // Signal the controller (PID 1 in its namespace) first so tap/firewall cleanup runs.
    let pid = child.pid
    const processes = execFileSync('ps', ['-eo', 'pid=,ppid=,comm='], { encoding: 'utf8' }).trim().split('\n').map(line => line.trim().split(/\s+/))
    for (let depth = 0; depth < 5; depth++) {
      const row = processes.find(row => Number(row[0]) === pid)
      assert.ok(row, 'Controller supervisor is missing')
      if (row[2] === 'leo')
        break
      const next = processes.find(row => Number(row[1]) === pid)
      assert.ok(next, 'Controller process must still be supervised')
      pid = Number(next[0])
    }
    execFileSync('sudo', ['-n', 'kill', '-TERM', String(pid)])
    await until(() => child.exitCode !== null || child.signalCode !== null)
  }
  async function start(port, code, leased = false) {
    const attempt = randomUUID()
    const plan = { id: attempt, runId: run, expires: Date.now() + 240000, resources: { cpu: 1, memoryMiB: 512, diskMiB: 1024 }, nodeLeaseRequired: leased, sandbox: 'yolo', cwd: workspace, command: ['/usr/local/bin/node', '-e', `process.chdir(${JSON.stringify(workspace)});${code}`], imports: [{ source: workspace, target: workspace }, { source: home, target: '/home/node' }] }
    await writeFile(path.join(data, 'runner-plans', `${attempt}.json`), JSON.stringify(plan))
    if (leased)
      await request(port, `/runs/${attempt}/lease`, 'POST', { remainingMs: 8000 })
    await request(port, `/runs/${attempt}`, 'POST')
    return attempt
  }
  try {
    for (const directory of [workspace, home, path.join(data, 'runner-plans')]) await mkdir(directory, { recursive: true })
    await writeFile(path.join(data, 'runner-secret'), 'fixture-node-controller')
    await copyFile(path.join(assets, 'busybox.tar'), path.join(workspace, 'busybox.tar'))
    const source = await controller('source', 44311)
    const first = await start(44311, `const fs=require('fs'); const cp=require('child_process');fs.mkdirSync('/home/node/.local/bin',{recursive:true});fs.writeFileSync('/home/node/.local/bin/installed-tool','retained tool');fs.mkdirSync('/home/node/.codex/sessions',{recursive:true});fs.writeFileSync('/home/node/.codex/sessions/fixture.json','native session bytes');fs.writeFileSync('untracked.bin',Buffer.alloc(100000,73));cp.execFileSync('docker',['load','-i','busybox.tar'],{timeout:30000});cp.execFileSync('docker',['run','--rm','--network=none','-v','leo-recovery-fixture:/state','busybox:1.37','sh','-c','echo docker-volume-survived > /state/probe'],{timeout:120000});let counter=0;setInterval(()=>fs.writeFileSync('counter',String(++counter)),100);console.log('fixture.ready');`)
    let logs = ''
    let ended = false
    const reading = (async () => {
      const r = await request(44311, `/runs/${first}/logs`)
      for await (const chunk of r.body) logs += Buffer.from(chunk).toString()
      ended = true
    })()
    await until(() => {
      assert.ok(!ended, `Probe exited: ${logs.split('\n').flatMap((line) => {
        try {
          const event = JSON.parse(line)
          return event.data ? [Buffer.from(event.data, 'base64').toString()] : []
        }
        catch { return [] }
      }).join('').slice(-3000)}`)
      return logs.includes(Buffer.from('fixture.ready').toString('base64').slice(0, 12))
    })
    let snapshot = await (await request(44311, `/runs/${first}/snapshot`, 'POST')).json()
    process.stdout.write(`${JSON.stringify({ stage: 'captured', pauseMs: snapshot.manifest.pauseMs, indexMs: snapshot.manifest.indexMs })}\n`)
    for (const block of snapshot.manifest.blocks) {
      if (!block.hash || blocks.has(block.hash))
        continue
      const bytes = Buffer.from(await (await request(44311, `/snapshots/${snapshot.id}/${block.hash}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(bytes).digest('hex'), block.hash)
      blocks.set(block.hash, bytes)
    }
    const baseBytes = [...blocks.values()].reduce((sum, b) => sum + b.length, 0)
    const firstHashes = new Set(blocks.keys())
    await request(44311, `/snapshots/${snapshot.id}/discard`, 'DELETE')
    await delay(1000)
    snapshot = await (await request(44311, `/runs/${first}/snapshot`, 'POST')).json()
    assert.ok(snapshot.manifest.blocks.some(block => firstHashes.has(block.hash)), 'Incremental capture must reuse unchanged data')
    let incrementalBytes = 0
    for (const block of snapshot.manifest.blocks) {
      if (!block.hash || blocks.has(block.hash))
        continue
      const bytes = Buffer.from(await (await request(44311, `/snapshots/${snapshot.id}/${block.hash}`)).arrayBuffer())
      assert.equal(createHash('sha256').update(bytes).digest('hex'), block.hash)
      blocks.set(block.hash, bytes)
      incrementalBytes += bytes.length
    }
    process.stdout.write(`${JSON.stringify({ stage: 'incremental', baseBytes, incrementalBytes, localBytesRead: snapshot.manifest.localBytesRead })}\n`)
    await request(44311, `/runs/${first}`, 'DELETE')
    await reading
    const after = await (await request(44311, `/runs/${first}/wait`, 'POST')).json()
    assert.equal(after.StatusCode, 143)
    await request(44311, `/snapshots/${snapshot.id}/discard`, 'DELETE')
    await stopController(source.child)
    // Separate controller state restores only through authenticated block fetches.
    server = createServer((req, res) => {
      const bytes = blocks.get(req.url.split('/').pop())
      if (req.headers.authorization !== 'Bearer fixture-restore-grant' || !bytes) {
        res.writeHead(403).end()
        return
      }
      res.writeHead(200, { 'content-length': bytes.length })
      res.end(bytes)
    })
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
    await controller('destination', 44312)
    await request(44312, `/disks/${run}/restore`, 'POST', { manifest: snapshot.manifest, master: `http://127.0.0.1:${server.address().port}`, grant: 'fixture-restore-grant', backupId: randomUUID() })
    const second = await start(44312, `const fs=require('fs');const assert=require('assert/strict');const cp=require('child_process');assert.equal(fs.readFileSync('/home/node/.local/bin/installed-tool','utf8'),'retained tool');assert.equal(fs.readFileSync('/home/node/.codex/sessions/fixture.json','utf8'),'native session bytes');assert.equal(fs.readFileSync('untracked.bin').length,100000);assert.ok(Number(fs.readFileSync('counter','utf8'))>0);assert.match(cp.execFileSync('docker',['run','--rm','--network=none','-v','leo-recovery-fixture:/state','busybox:1.37','cat','/state/probe'],{encoding:'utf8',timeout:30000}),/docker-volume-survived/);console.log('restore.verified');`)
    const restored = await (await request(44312, `/runs/${second}/wait`, 'POST')).json()
    assert.equal(restored.StatusCode, 0, JSON.stringify(restored))
    const leased = await start(44312, `setInterval(()=>console.log('leased.tick'),100)`, true)
    const fenced = await (await request(44312, `/runs/${leased}/wait`, 'POST')).json()
    assert.equal(fenced.StatusCode, 75, 'An expired lease must stop the complete VM as a recoverable interruption')
    process.stdout.write(`${JSON.stringify({ stage: 'lease-expired', exitCode: fenced.StatusCode })}\n`)
    process.stdout.write(`${JSON.stringify({ capturePauseMs: snapshot.manifest.pauseMs, indexMs: snapshot.manifest.indexMs, transferredBlocks: blocks.size, transferredBytes: [...blocks.values()].reduce((sum, b) => sum + b.length, 0), restored: ['untracked files', 'installed tool', 'session file bytes', 'Docker volume'] })}\n`)
  }
  finally {
    server?.closeAllConnections()
    server?.close()
    for (const child of children) {
      if (child.exitCode === null && child.signalCode === null) {
      // Killing the unshare parent kills the private PID namespace and all guest processes.
        try {
          await stopController(child)
        }
        catch {
          execFileSync('sudo', ['-n', 'kill', '-KILL', '--', `-${child.pid}`], { stdio: 'ignore' })
        }
      }
    }
    await delay(1500)
    execFileSync('sudo', ['-n', 'rm', '-rf', '--', root])
  }
}
main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
