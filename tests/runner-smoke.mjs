import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import { createServer as createHttpServer } from 'node:http'
import { createServer } from 'node:net'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { blockDigest } from './block-digest.mjs'
import { entrypointSmoke } from './runner-entrypoint-smoke.mjs'
import { prepareStorageOrigin, storageSmoke } from './runner-storage-smoke.mjs'

async function main() {
  const image = process.argv[2] || 'cairn-firecracker:dev'
  const docker = (...args) => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 180000, stdio: ['pipe', 'pipe', 'pipe'] }).trim()
  // The extracted guest image must stay on disk: /tmp can be a RAM filesystem.
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || '/var/tmp', 'cairn-microvm-'))
  const name = `cairn-vm-test-${randomUUID().slice(0, 8)}`
  const networkName = `${name}-network`
  const networkServer = `${name}-peer`
  // The generated installation reaches its manager by name on a private network.
  const installationNetwork = `${name}-installation`
  // A documentation-only subnet emulates public destinations without Internet access.
  const publicPeer = '203.0.113.3'
  const headers = { authorization: 'Bearer fixture-runner-token' }
  let url
  let auth
  let claudeAuth
  let mcp
  // Agents reach run-scoped MCP inside their VM, never through the manager origin.
  const vmMcp = 'http://127.0.0.1:5202'
  const mcpToken = 'fixture-mcp-run-token'
  const mcpCalls = []

  async function until(operation, timeout = 180000) {
    const deadline = Date.now() + timeout
    while (Date.now() < deadline) {
      const result = await operation()
      if (result)
        return result
      await setTimeout(100)
    }

    throw new Error('MicroVM test timed out')
  }

  async function api(endpoint, method = 'GET', body) {
    const response = await fetch(url + endpoint, {
      method,
      headers: { ...headers, 'content-type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(180000),
    })
    assert.equal(response.ok, true, `${method} ${endpoint}: ${response.status}`)
    return response
  }

  async function reconnect() {
    url = `http://${await until(() => {
      try {
        return docker('port', name, '4311/tcp')
      }
      catch { return false }
    }, 10000)}`
    await until(() => fetch(`${url}/health`).then(r => r.ok).catch(() => false))
  }

  try {
    for (const dir of ['data/runner-plans', 'state'])
      await mkdir(path.join(root, dir), { recursive: true })
    await writeFile(path.join(root, 'data/runner-secret'), 'fixture-runner-token')
    await writeFile(path.join(root, 'data/private-manager-canary'), 'must-not-enter-guest')
    const projectId = randomUUID()
    const runId = randomUUID()
    const runRoot = `/data/runs/${runId}`
    const source = path.join(root, 'data/runs', runId)
    for (const dir of ['workspace', 'home/.codex', 'home/.claude', 'chat-input', 'output'])
      await mkdir(path.join(source, dir), { recursive: true })
    await writeFile(path.join(source, 'home/.codex/cairn-managed-auth'), '1')
    await writeFile(path.join(source, 'home/.claude/.credentials.json'), JSON.stringify({ fixture: 'provisioned' }))
    await writeFile(path.join(source, 'chat-input/messages.json'), '[]')
    await writeFile(path.join(source, 'workspace/nested-kvm.c'), await readFile(new URL('./fixtures/nested-kvm.c', import.meta.url)))
    const networkProbe = await readFile(new URL('./fixtures/vm-network.mjs', import.meta.url))
    await writeFile(path.join(source, 'workspace/network-probe.mjs'), networkProbe)
    docker('network', 'create', '--internal', '--subnet', '203.0.113.0/29', networkName)
    docker('run', '-d', '--name', networkServer, '--user', '0:0', '-v', `${source}/workspace/network-probe.mjs:/network-probe.mjs:ro`, '--entrypoint', '/usr/local/bin/node', image, '/network-probe.mjs', 'server')
    const privatePeer = docker('inspect', '--format', '{{(index .NetworkSettings.Networks "bridge").IPAddress}}', networkServer)
    docker('network', 'connect', '--ip', publicPeer, networkName, networkServer)
    const fixture = `
import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import { execFileSync } from 'node:child_process';
import { DatabaseSync } from 'node:sqlite';
const root=${JSON.stringify(runRoot)};
const mode=process.argv[2];
if(mode!=='first'){const disk=fs.statfsSync(root+'/workspace');assert.ok(disk.blocks*disk.bsize>32*1024**3,'grown workspace exceeds the old jailer file limit');}
assert.ok(Math.abs(Date.now()-Number(process.argv[3])) < 30000,'clock repaired before execution');
const project=root+'/workspace/'+${JSON.stringify(projectId)};
if(mode==='first')execFileSync('cc',['-Wall','-Wextra','-Werror','-O2',root+'/workspace/nested-kvm.c','-o',root+'/workspace/nested-kvm'],{timeout:30000});
if(mode==='first'||mode==='resume')assert.match(execFileSync(root+'/workspace/nested-kvm',[],{encoding:'utf8',timeout:15000}),/KVM_RUN, rax=42, HLT/);
assert.match(execFileSync('uname',['-r'],{encoding:'utf8'}),/^6\\.12\\.109/);
assert.equal(fs.existsSync('/sys/bus/serio/devices/serio0'),false,'guest has no emulated PS/2 keyboard');
if(mode==='first'){
  const templates='/opt/cairn-codex-state';
  const manifest=JSON.parse(fs.readFileSync(templates+'/manifest.json','utf8'));
  assert.equal(manifest.version,1);
  assert.ok(manifest.files.some(name=>/^state_\\d+\\.sqlite$/.test(name)));
  for(const name of manifest.files){
    const db=new DatabaseSync(templates+'/'+name,{readOnly:true});
    assert.equal(db.prepare('PRAGMA integrity_check').get().integrity_check,'ok');
    for(const {name:table} of db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'").all()){
      if(table==='_sqlx_migrations')continue;
      assert.match(table,/^\\w+$/);
      assert.equal(db.prepare('SELECT count(*) AS count FROM '+table).get().count,0,'template has no runtime rows');
    }
    db.close();
  }
}
for(const path of ['/data/private-manager-canary','/data/runner-secret','/var/run/docker.sock'])assert.equal(fs.existsSync(path),false,path);
assert.equal(process.env.RUNNER_TOKEN,undefined);
if(mode==='first') {
  console.log(execFileSync('/usr/local/bin/node',[root+'/workspace/network-probe.mjs','guest',${JSON.stringify(publicPeer)},${JSON.stringify(privatePeer)}],{encoding:'utf8',timeout:15000}));
  const env=JSON.parse(execFileSync('/usr/local/bin/cairn',['toolkit-env'],{encoding:'utf8'}));
  assert.equal(env.CAIRN_TOOLKIT_DIR,'/opt/cairn-toolkit');
  for(const tool of ['cargo','rustc','pnpm','python','uv','rg','fd','gh','codex','claude'])execFileSync(tool,['--version'],{env,timeout:30000});
  const auth=await new Promise((resolve,reject)=>{const socket=net.connect('/run/cairn-auth.sock',()=>socket.write('{"refresh":false}\\n'));let data='';socket.on('data',chunk=>{data+=chunk;if(data.includes('\\n')){socket.end();resolve(JSON.parse(data))}});socket.on('error',reject);});
  assert.equal(auth.accessToken,'fixture-access-token');
  assert.equal(fs.existsSync(project),false,'unopened project is absent');
  fs.writeFileSync('/tmp/artifact-probe.bin',Buffer.alloc(150000,90));
  fs.symlinkSync('/etc/passwd','/tmp/artifact-link');
  console.log('probe.ready');
  const deadline=Date.now()+20000;
  while(!fs.readFileSync('/run/cairn-chat/messages.json','utf8').includes('steered')){assert.ok(Date.now()<deadline,'live inbox');await new Promise(r=>setTimeout(r,100));}
  assert.equal(fs.readFileSync(project+'/hello','utf8'),'imported on demand');
  fs.writeFileSync(project+'/hello','guest edit');
  console.log('project.edited');
  while(!fs.readFileSync('/run/cairn-chat/messages.json','utf8').includes('reopened')){assert.ok(Date.now()<deadline,'reopen response');await new Promise(r=>setTimeout(r,100));}
  assert.equal(fs.readFileSync(project+'/hello','utf8'),'guest edit','reopen must preserve edits');
  console.log(execFileSync('docker',['run','--rm','busybox:1.37','echo','nested-docker-ok'],{encoding:'utf8',timeout:120000}));
  fs.writeFileSync(root+'/workspace/compose.yaml',JSON.stringify({services:{probe:{image:'busybox:1.37',command:['echo','compose-ok']}}}));
  assert.match(execFileSync('docker',['compose','-f',root+'/workspace/compose.yaml','run','--rm','probe'],{encoding:'utf8',timeout:30000}),/compose-ok/);
  execFileSync('docker',['compose','-f',root+'/workspace/compose.yaml','down'],{timeout:30000});
  fs.writeFileSync(root+'/workspace/preserved','uncommitted work');
  fs.writeFileSync('/home/node/.codex/archive-session-probe.json','preserved session state');
} else {
  assert.equal(fs.readFileSync(root+'/workspace/preserved','utf8'),'uncommitted work');
  assert.equal(fs.readFileSync('/home/node/.codex/archive-session-probe.json','utf8'),'preserved session state');
  assert.equal(fs.readFileSync(project+'/hello','utf8'),'guest edit','on-demand project survives restart');
  console.log(execFileSync('docker',['run','--rm','--pull=never','busybox:1.37','echo','cached-docker-ok'],{encoding:'utf8',timeout:30000}));
}
if(mode==='cancel'||mode==='crash') {
  fs.writeFileSync(root+'/workspace/interrupted','saved before interruption');
  execFileSync('sync');
  console.log('probe.pause');
  if(mode==='cancel') {
    const deadline=Date.now()+30000;
    while(!fs.readFileSync('/run/cairn-chat/messages.json','utf8').includes('capture-update')){assert.ok(Date.now()<deadline,'capture update request');await new Promise(r=>setTimeout(r,100));}
    fs.writeFileSync(root+'/workspace/capture-update',Buffer.alloc(4*1024*1024,91));
    execFileSync('sync');
    console.log('probe.updated');
  }
  await new Promise(()=>{setInterval(()=>{},1000)});
}
if(mode==='first'||mode==='resume'){
  const call=async(path,token)=>fetch(${JSON.stringify(vmMcp)}+path,{method:'POST',headers:{authorization:'Bearer '+token,'content-type':'application/json',accept:'application/json, text/event-stream','mcp-protocol-version':'2025-11-25'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'list_nodes',arguments:{}}}),signal:AbortSignal.timeout(15000)});
  const response=await call('/mcp-workspace',${JSON.stringify(mcpToken)});
  assert.equal(response.status,200,'cairn_workspace is reachable from the VM');
  const reply=await response.json();
  assert.equal(reply.result.structuredContent.mode,mode,'the run channel answers this attempt');
  assert.equal((await call('/mcp-workspace','wrong-token')).status,401,'the manager still checks the run token');
  console.log('mcp.workspace.'+mode);
}
if(mode==='recover')assert.equal(fs.readFileSync(root+'/workspace/interrupted','utf8'),'saved before interruption');
if(mode==='managed-claude') {
  const state=await new Promise((resolve,reject)=>{const socket=net.connect('/run/cairn-auth.sock',()=>socket.write('{}\\n'));let data='';socket.on('data',chunk=>{data+=chunk;if(data.includes('\\n')){socket.end();resolve(JSON.parse(data))}});socket.on('error',reject);});
  assert.equal(state.claudeAiOauth.accessToken,'fixture-claude-access');
  assert.equal(state.claudeAiOauth.refreshToken,undefined);
  fs.writeFileSync('/home/node/.claude/.credentials.json',JSON.stringify({fixture:'must-not-overwrite-shared-state'}));
}
fs.writeFileSync(root+'/output/result.md','guest test passed '+mode);
console.log('probe.done');
`
    await writeFile(path.join(source, 'workspace/probe.mjs'), fixture)
    auth = createServer(socket => socket.once('data', () => socket.end('{"accessToken":"fixture-access-token"}\n')))
    await new Promise(resolve => auth.listen(path.join(source, 'home/.codex/cairn-auth.sock'), resolve))
    claudeAuth = createServer(socket => socket.once('data', () => socket.end(`${JSON.stringify({ claudeAiOauth: { accessToken: 'fixture-claude-access', expiresAt: Date.now() + 3600000 } })}\n`)))
    await new Promise(resolve => claudeAuth.listen(path.join(source, 'home/.claude/cairn-auth.sock'), resolve))
    // The manager's run channel, as served during an attempt in the run home.
    mcp = createHttpServer((request, response) => {
      let body = ''
      request.on('data', chunk => body += chunk)
      request.on('end', () => {
        const message = JSON.parse(body)
        mcpCalls.push({
          method: request.method,
          path: request.url,
          authorization: request.headers.authorization,
          tool: message.params?.name,
        })
        if (request.headers.authorization !== `Bearer ${mcpToken}`) {
          response.writeHead(401, { 'content-type': 'application/json' }).end('{"error":"MCP run access expired or was revoked."}')
          return
        }

        const mode = mcpCalls.filter(call => call.authorization === `Bearer ${mcpToken}`).length === 1 ? 'first' : 'resume'
        const result = { content: [{ type: 'text', text: mode }], structuredContent: { mode } }
        response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ jsonrpc: '2.0', id: message.id, result }))
      })
    })
    await new Promise(resolve => mcp.listen(path.join(source, 'home/cairn-mcp.sock'), resolve))
    docker('run', '-d', '--name', name, '--user', '0:0', '--read-only', '--cap-drop', 'ALL', ...['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'].flatMap(cap => ['--cap-add', cap]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', '--device', '/dev/kvm', '--device', '/dev/fuse', '--device', '/dev/net/tun', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', '6g', '--cpus', '3', '-e', 'CONCURRENCY=5', '--entrypoint', '/usr/local/bin/cairn', image, 'runner-broker')
    docker('network', 'connect', '--ip', '203.0.113.2', networkName, name)
    docker('network', 'create', installationNetwork)
    docker('network', 'connect', '--alias', 'manager', installationNetwork, name)
    // First prove every listener is reachable outside the guest firewall.
    process.stdout.write(docker('exec', name, '/usr/local/bin/node', `${runRoot}/workspace/network-probe.mjs`, 'control', publicPeer, privatePeer))
    await reconnect()
    const storageFixture = await prepareStorageOrigin({
      root,
      docker,
      name,
      api,
      managerOrigin: 'http://manager:4310',
    })
    const { storage } = storageFixture
    const initialHealth = await (await api('/health')).json()
    assert.equal(initialHealth.sharedResources, true)
    assert.equal(initialHealth.budget.slots, 5)
    const originalBudget = initialHealth.budget
    const adjustedBudget = { slots: 6, limits: { ...originalBudget.limits, cpu: 2, memoryMiB: 4096 } }
    const unauthenticatedBudget = await fetch(`${url}/node-budget`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(adjustedBudget),
    })
    assert.equal(unauthenticatedBudget.status, 401)
    await api('/node-budget', 'POST', adjustedBudget)
    assert.deepEqual((await (await api('/health')).json()).budget, adjustedBudget)
    assert.equal(docker('exec', name, 'cat', '/run/cairn-cgroup/cairn-shared/cpu.max'), '200000 100000')
    assert.equal(docker('exec', name, 'cat', '/run/cairn-cgroup/cairn-shared/memory.max'), String(4096 * 1024 ** 2))
    await api('/node-budget', 'POST', originalBudget)
    for (const mode of ['first', 'resume', 'cancel', 'crash', 'recover', 'managed-claude', 'codex-return']) {
      const id = randomUUID()
      const plan = {
        id,
        runId,
        storage,
        resources: { cpu: 2, memoryMiB: 4096, diskMiB: mode === 'first' ? 1024 : 65536 },
        expires: Date.now() + 300000,
        sandbox: 'yolo',
        cwd: `${runRoot}/workspace`,
        command: ['/usr/local/bin/node', `${runRoot}/workspace/probe.mjs`, mode, Date.now().toString()],
        chat: { output: `${runRoot}/output/result.md` },
        imports: [
          { source: `${runRoot}/workspace`, target: `${runRoot}/workspace`, readOnly: false },
          { source: `${runRoot}/home`, target: '/home/node', readOnly: false },
          { source: `${runRoot}/output`, target: `${runRoot}/output`, readOnly: false },
          { source: `${runRoot}/chat-input`, target: '/run/cairn-chat', readOnly: true },
        ],
      }
      if (mode === 'managed-claude') {
        plan.chat.provider = 'claude'
        plan.chat.claudeManagedAuth = true
      }

      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      const start = Date.now()
      await api(`/runs/${id}`, 'POST')
      const output = (async () => {
        const response = await api(`/runs/${id}/logs`)
        let pending = ''
        let text = ''
        try {
          for await (const chunk of response.body) {
            pending += Buffer.from(chunk).toString()
            while (pending.includes('\n')) {
              const end = pending.indexOf('\n')
              const event = JSON.parse(pending.slice(0, end))
              pending = pending.slice(end + 1)
              if (event.type !== 'output')
                continue
              const data = Buffer.from(event.data, 'base64').toString()
              text += data
              process.stdout.write(data)
              if (data.includes('probe.ready')) {
                const exported = await api(`/runs/${id}/artifact`, 'POST', { runId, path: '/tmp/artifact-probe.bin' })
                assert.deepEqual(Buffer.from(await exported.arrayBuffer()), Buffer.alloc(150000, 90))
                for (const file of ['/etc/passwd', '/tmp/artifact-link', '/tmp/../etc/passwd', '/dev/zero']) {
                  const denied = await fetch(`${url}/runs/${id}/artifact`, { method: 'POST', headers: { ...headers, 'content-type': 'application/json' }, body: JSON.stringify({ runId, path: file }) })
                  assert.equal(denied.status, 400, file)
                }

                const wrongRun = await fetch(`${url}/runs/${id}/artifact`, { method: 'POST', headers: { ...headers, 'content-type': 'application/json' }, body: JSON.stringify({ runId: randomUUID(), path: '/tmp/artifact-probe.bin' }) })
                assert.equal(wrongRun.status, 403)

                const directory = path.join(source, 'workspace', projectId)
                await mkdir(directory, { recursive: true })
                await writeFile(path.join(directory, 'hello'), 'imported on demand')
                const body = { runId, source: `${runRoot}/workspace/${projectId}`, target: `${runRoot}/workspace/${projectId}` }
                const denied = await fetch(`${url}/runs/${id}/projects/${projectId}`, { method: 'POST', headers: { ...headers, 'content-type': 'application/json' }, body: JSON.stringify({ ...body, runId: randomUUID() }) })
                assert.equal(denied.status, 409)
                const opened = await (await api(`/runs/${id}/projects/${projectId}`, 'POST', body)).json()
                assert.deepEqual(opened, { ok: true, reused: false })
                await writeFile(path.join(source, 'chat-input/messages.json'), '[{"text":"steered"}]')
              }

              if (data.includes('project.edited')) {
                const body = { runId, source: `${runRoot}/workspace/${projectId}`, target: `${runRoot}/workspace/${projectId}` }
                const opened = await (await api(`/runs/${id}/projects/${projectId}`, 'POST', body)).json()
                assert.deepEqual(opened, { ok: true, reused: true })
                await writeFile(path.join(source, 'chat-input/messages.json'), '[{"text":"reopened"}]')
              }

              if (data.includes('probe.pause') && mode === 'cancel') {
                const captureStarted = Date.now()
                const point = await (await api(`/runs/${id}/snapshot`, 'POST')).json()
                const firstCaptureMs = Date.now() - captureStarted
                assert.ok(point.manifest.size > 0)
                assert.equal(point.manifest.onDemand, true)
                const block = point.manifest.blocks.find(block => block.hash)
                assert.ok(block, 'Active capture must contain durable guest data')
                const bytes = Buffer.from(await (await api(`/snapshots/${point.id}/${block.hash}`)).arrayBuffer())
                assert.equal(blockDigest(bytes, block.hash), block.hash)
                // New writes enter the next journal generation; the sealed one stays readable.
                await writeFile(path.join(source, 'chat-input/messages.json'), '[{"text":"capture-update"}]')
                await until(() => docker('exec', name, 'cat', `/runner-state/${id}.log`).split('\n').filter(Boolean).some(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString().includes('probe.updated')))
                const original = Buffer.from(await (await api(`/snapshots/${point.id}/${block.hash}`)).arrayBuffer())
                assert.deepEqual(original, bytes, 'The sealed capture stays immutable while the guest writes')
                // Starting the next capture retires the previous capture handle.
                const nextStarted = Date.now()
                const next = await (await api(`/runs/${id}/snapshot`, 'POST')).json()
                const nextCaptureMs = Date.now() - nextStarted
                assert.equal(next.manifest.onDemand, true)
                assert.ok(next.manifest.generation > point.manifest.generation, 'New writes advance the journal generation')
                assert.equal(next.manifest.blocks.length, point.manifest.blocks.length)
                const changed = next.manifest.blocks.find((block, index) => block.hash && block.hash !== point.manifest.blocks[index].hash)
                assert.ok(changed, 'The next generation includes guest writes')
                const written = Buffer.from(await (await api(`/snapshots/${next.id}/${changed.hash}`)).arrayBuffer())
                assert.equal(blockDigest(written, changed.hash), changed.hash)
                docker('exec', name, 'test', '!', '-e', `/runner-state/disks/${runId}/data.ext4`)
                await api(`/snapshots/${next.id}/discard`, 'DELETE')
                process.stdout.write(`${JSON.stringify({
                  mode: 'active-capture',
                  firstCaptureMs,
                  nextCaptureMs,
                  generation: next.manifest.generation,
                  status: 'passed',
                })}\n`)
                await api(`/runs/${id}`, 'DELETE')
              }

              if (text.includes('probe.pause') && mode === 'crash') {
                docker('kill', '--signal', 'KILL', name)
                docker('start', name)
                // The test origin is an exec process, so container restart killed it too.
                docker('exec', '-d', name, '/usr/local/bin/node', '/data/storage-fixture/server.mjs')
                await reconnect()
              }
            }
          }
        }
        catch (error) {
          if (mode !== 'crash' || !text.includes('probe.pause'))
            throw error
        }

        return text
      })()
      const waiting = async () => (await api(`/runs/${id}/wait`, 'POST')).json()
      const [initialStatus, text] = await Promise.all([
        waiting().catch((error) => {
          if (mode !== 'crash')
            throw error
          return { StatusCode: 143 }
        }),
        output,
      ])
      let status = initialStatus
      if (mode === 'crash')
        status = await waiting()
      assert.equal(status.StatusCode, ['cancel', 'crash'].includes(mode) ? 143 : 0, text)
      if (['cancel', 'crash'].includes(mode))
        assert.ok(text.includes('probe.pause'))
      else
        assert.equal(docker('exec', name, 'cat', `${runRoot}/output/result.md`), `guest test passed ${mode}`)
      if (['first', 'resume'].includes(mode)) {
        assert.ok(text.includes(`mcp.workspace.${mode}`), text)
        assert.deepEqual(mcpCalls.at(-1), {
          method: 'POST',
          path: '/mcp-workspace',
          authorization: 'Bearer wrong-token',
          tool: 'list_nodes',
        })
      }

      if (mode === 'managed-claude') {
        // Guest credential writes never reach the host.
        assert.equal(JSON.parse(await readFile(path.join(source, 'home/.claude/.credentials.json'), 'utf8')).fixture, 'provisioned')
        assert.ok(!text.includes('fixture-claude-access'), 'access snapshot stays out of logs')
      }

      assert.equal(await readFile(path.join(source, 'workspace/preserved'), 'utf8').catch(() => null), null, 'guest edits must not affect host checkout')
      await api(`/runs/${id}`, 'DELETE')
      process.stdout.write(`${JSON.stringify({ mode, durationMs: Date.now() - start, status: 'passed' })}\n`)
    }

    await entrypointSmoke({ root, api, storage })

    // Independent disks must run concurrently and enforce each guest policy.
    const probes = []
    for (const sandbox of ['read-only', 'workspace-write']) {
      const runId = randomUUID()
      const id = randomUUID()
      const workspace = `/data/runs/${runId}/workspace`
      const lazyId = randomUUID()
      const lazy = `/data/runs/${runId}/projects/${lazyId}`
      const directory = path.join(root, 'data/runs', runId, 'workspace')
      const inbox = path.join(root, 'data/runs', runId, 'chat-input')
      await mkdir(directory, { recursive: true })
      await mkdir(inbox, { recursive: true })
      await writeFile(path.join(inbox, 'messages.json'), '[]')
      await writeFile(path.join(directory, 'sentinel'), sandbox)
      await writeFile(path.join(directory, 'policy-probe.mjs'), await readFile(new URL('./fixtures/vm-policy.mjs', import.meta.url)))
      const plan = {
        id,
        runId,
        storage,
        resources: { cpu: 1, memoryMiB: 512, diskMiB: 512 },
        expires: Date.now() + 60000,
        sandbox,
        cwd: workspace,
        command: ['/usr/local/bin/node', `${workspace}/policy-probe.mjs`, workspace, lazy, sandbox],
        imports: [
          { source: workspace, target: workspace, readOnly: sandbox === 'read-only' },
          { source: `/data/runs/${runId}/chat-input`, target: '/run/cairn-chat', readOnly: true },
        ],
      }
      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      await api(`/runs/${id}`, 'POST')
      probes.push({
        id,
        runId,
        sandbox,
        directory,
        lazyId,
        lazy,
        inbox,
      })
    }

    const outcomes = await Promise.allSettled(probes.map(async ({
      id,
      runId,
      sandbox,
      directory,
      lazyId,
      lazy,
      inbox,
    }) => {
      await until(async () => {
        let logs
        try {
          logs = docker('exec', name, 'cat', `/runner-state/${id}.log`)
        }
        catch {
          return false
        }

        return logs.split('\n').filter(Boolean).some((line) => {
          const row = JSON.parse(line)
          return row.type === 'output' && Buffer.from(row.data, 'base64').toString().includes('parallel.ready')
        })
      })
      const lazySource = path.join(root, 'data/runs', runId, 'projects', lazyId)
      await mkdir(lazySource, { recursive: true })
      await writeFile(path.join(lazySource, 'sentinel'), 'lazy')
      await api(`/runs/${id}/projects/${lazyId}`, 'POST', { runId, source: lazy, target: lazy })
      await writeFile(path.join(inbox, 'messages.json'), '[{"text":"import-confirmed"}]')
      const response = await api(`/runs/${id}/logs`)
      const records = (await response.text()).trim().split('\n').map(line => JSON.parse(line))
      const output = records.filter(row => row.type === 'output').map(row => Buffer.from(row.data, 'base64').toString()).join('')
      const status = await (await api(`/runs/${id}/wait`, 'POST')).json()
      assert.equal(status.StatusCode, 0, output)
      assert.match(output, /parallel.ready[\s\S]*parallel.done/)
      assert.equal(await readFile(path.join(directory, 'created'), 'utf8').catch(() => null), null)
      process.stdout.write(`${JSON.stringify({ mode: sandbox, status: 'passed' })}\n`)
    }))
    for (const outcome of outcomes) {
      if (outcome.status === 'rejected')
        throw outcome.reason
    }

    const restricted = probes.find(probe => probe.sandbox === 'read-only')
    const previous = JSON.parse(await readFile(path.join(root, 'data/runner-plans', `${restricted.id}.json`), 'utf8'))
    const resumedId = randomUUID()
    const code = `const fs=require('node:fs'),assert=require('node:assert/strict');assert.equal(fs.readFileSync(${JSON.stringify(`${restricted.lazy}/sentinel`)},'utf8'),'lazy');assert.throws(()=>fs.writeFileSync(${JSON.stringify(`${restricted.lazy}/changed`)},'no'),e=>e.code==='EROFS');console.log('policy.resumed');`
    const resumed = {
      ...previous,
      id: resumedId,
      expires: Date.now() + 60000,
      command: ['/usr/local/bin/node', '-e', code],
    }
    await writeFile(path.join(root, 'data/runner-plans', `${resumedId}.json`), JSON.stringify(resumed))
    await api(`/runs/${resumedId}`, 'POST')
    const status = await (await api(`/runs/${resumedId}/wait`, 'POST')).json()
    assert.equal(status.StatusCode, 0, 'lazy read-only policy survives VM restart')
    process.stdout.write(`${JSON.stringify({ mode: 'read-only-resume', status: 'passed' })}\n`)
    // Five running VMs occupy the configured slots. A sixth cannot enter.
    await until(async () => (await (await api('/health')).json()).activeRuns === 0)
    const held = []
    for (let index = 0; index < 6; index++) {
      const id = randomUUID()
      const runId = randomUUID()
      const cwd = `/data/runs/${runId}/workspace`
      const directory = path.join(root, 'data/runs', runId, 'workspace')
      await mkdir(directory, { recursive: true })
      const plan = {
        id,
        runId,
        storage,
        resources: { cpu: 1, memoryMiB: 512, diskMiB: 512 },
        expires: Date.now() + 60000,
        sandbox: 'yolo',
        cwd,
        command: ['/usr/local/bin/node', '-e', `
          const assert=require('node:assert/strict'),os=require('node:os');
          assert.equal(os.cpus().length,${Math.min(originalBudget.limits.cpu, 32)});
          const ceiling=${(originalBudget.limits.memoryMiB - 512) * 1024 ** 2};
          assert.ok(os.totalmem()<=ceiling,'guest leaves controller memory headroom');
          console.log('slot.ready');setInterval(()=>{},1000);
        `],
        imports: [{ source: cwd, target: cwd }],
      }
      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      const response = await fetch(`${url}/runs/${id}`, { method: 'POST', headers })
      assert.equal(response.status, index < 5 ? 200 : 503)
      if (index < 5)
        held.push(id)
      if (index === 0) {
        const duplicate = randomUUID()
        await writeFile(path.join(root, 'data/runner-plans', `${duplicate}.json`), JSON.stringify({ ...plan, id: duplicate }))
        const denied = await fetch(`${url}/runs/${duplicate}`, { method: 'POST', headers })
        assert.equal(denied.status, 409, 'same disk cannot enter twice')
      }
    }

    assert.equal((await (await api('/health')).json()).pool.occupied, 5)
    await until(async () => {
      const states = JSON.parse(docker('exec', name, '/usr/local/bin/node', '-e', `
        const fs=require('node:fs');
        const states=${JSON.stringify(held)}.map(id=>{
          const prefix='/runner-state/'+id;
          const logs=fs.existsSync(prefix+'.log')?fs.readFileSync(prefix+'.log','utf8'):'';
          const output=logs.split('\\n').filter(Boolean).map(line=>Buffer.from(JSON.parse(line).data||'','base64').toString()).join('');
          return {id,output,exit:fs.existsSync(prefix+'.exit')?fs.readFileSync(prefix+'.exit','utf8'):null};
        });
        console.log(JSON.stringify(states));
      `))
      for (const state of states) {
        assert.equal(state.exit, null, `Held VM ${state.id} exited before capacity validation: ${state.output}`)
      }

      return states.every(state => state.output.includes('slot.ready'))
    })
    const sharedHealth = await (await api('/health')).json()
    assert.ok(sharedHealth.usage.memoryMiB < originalBudget.limits.memoryMiB)
    const balloons = JSON.parse(docker('exec', name, '/usr/local/bin/node', '-e', `
      const fs=require('node:fs'),http=require('node:http');
      const root='/runner-state/jails/firecracker';
      Promise.all(fs.readdirSync(root).map(async id=>{
        const jail=root+'/'+id+'/root';
        const config=JSON.parse(fs.readFileSync(jail+'/config.json'));
        const stats=await new Promise((resolve,reject)=>{
          const req=http.get({socketPath:jail+'/api.sock',path:'/balloon/statistics'},res=>{
            let data='';res.on('data',part=>data+=part);res.on('end',()=>resolve(JSON.parse(data)));
          });req.on('error',reject);
        });
        return {machine:config['machine-config'],config:config.balloon,stats};
      })).then(result=>console.log(JSON.stringify(result)));
    `))
    assert.equal(balloons.length, 5)
    for (const balloon of balloons) {
      // MemTotal excludes kernel reservations and can shrink with the balloon.
      // The VMM configuration proves the shared ceiling for every slot.
      assert.equal(balloon.machine.mem_size_mib, originalBudget.limits.memoryMiB - 512)
      assert.equal(balloon.machine.vcpu_count, Math.min(originalBudget.limits.cpu, 32))
      assert.equal(balloon.config.free_page_reporting, true)
      assert.equal(balloon.config.deflate_on_oom, true)
      assert.ok(balloon.stats.available_memory > 0, 'virtio balloon statistics reach Firecracker')
    }

    await api('/node-budget', 'POST', { ...originalBudget, slots: 3 })
    const reducedHealth = await (await api('/health')).json()
    assert.equal(reducedHealth.pool.capacity, 3)
    assert.equal(reducedHealth.pool.occupied, 5, 'lowering slots preserves active guests')
    const invalidBudget = await fetch(`${url}/node-budget`, {
      method: 'POST',
      headers: { ...headers, 'content-type': 'application/json' },
      body: JSON.stringify({ ...originalBudget, limits: { ...originalBudget.limits, memoryMiB: 128 } }),
    })
    assert.equal(invalidBudget.status, 400, 'the controller reserve cannot be the entire RAM budget')
    const tooSmall = await fetch(`${url}/node-budget`, {
      method: 'POST',
      headers: { ...headers, 'content-type': 'application/json' },
      body: JSON.stringify({ ...originalBudget, limits: { ...originalBudget.limits, memoryMiB: 640 } }),
    })
    assert.equal(tooSmall.status, 409, 'a RAM decrease cannot kill existing guests')
    assert.equal((await (await api('/health')).json()).pool.occupied, 5)
    await Promise.all(held.map(id => api(`/runs/${id}`, 'DELETE')))
    await until(async () => {
      const health = await (await api('/health')).json()
      const speculative = health.pool.ready + Number(health.pool.preparing)
      assert.ok(speculative <= 1, 'anonymous preparation stays bounded')
      return health.activeRuns === 0 && health.pool.occupied === speculative
    })
    await api('/node-budget', 'POST', originalBudget)
    process.stdout.write(`${JSON.stringify({ mode: 'pool-capacity-cancel-refill', capacity: 5, status: 'passed' })}\n`)
    await storageSmoke({
      root,
      docker,
      name,
      api,
      until,
      reconnect,
      storageFixture,
    })
  }
  catch (error) {
    console.error(error)
    try {
      console.error(docker('logs', name))
      console.error(docker('exec', name, 'sh', '-c', 'tail -n 60 /runner-state/*.boot.log'))
    }
    catch {
    }

    process.exitCode = 1
  }
  finally {
    if (process.env.KEEP_VM_TEST !== '1') {
      try {
        docker('rm', '-fv', name)
      }
      catch {
      }

      try {
        docker('rm', '-fv', networkServer)
        docker('network', 'rm', networkName)
      }
      catch {
      }

      try {
        docker('network', 'rm', installationNetwork)
      }
      catch {
      }
    }

    if (auth)
      await new Promise(resolve => auth.close(resolve))
    if (claudeAuth)
      await new Promise(resolve => claudeAuth.close(resolve))
    if (mcp)
      await new Promise(resolve => mcp.close(resolve))
    if (process.env.KEEP_VM_TEST !== '1') {
      docker('run', '--rm', '--user', '0:0', '-v', `${root}:/cleanup`, '--entrypoint', '/bin/rm', image, '-rf', '/cleanup/data', '/cleanup/state')
      await rm(root, { recursive: true, force: true })
    }
    else {
      process.stdout.write(`Evidence retained at ${root}\n`)
    }
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
