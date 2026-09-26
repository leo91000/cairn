import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { createHash, randomUUID } from 'node:crypto'
import { mkdir, mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises'
import { createServer } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

async function main() {
  const image = process.argv[2] || 'leo-firecracker:dev'
  const docker = (...args) => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 180000, stdio: ['pipe', 'pipe', 'pipe'] }).trim()
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || os.tmpdir(), 'leo-microvm-'))
  const name = `leo-vm-test-${randomUUID().slice(0, 8)}`
  const networkName = `${name}-network`
  const networkServer = `${name}-peer`
  // A documentation-only subnet emulates public destinations without Internet access.
  const publicPeer = '203.0.113.3'
  const headers = { authorization: 'Bearer fixture-runner-token' }
  let url
  let auth
  let claudeAuth
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
    const response = await fetch(url + endpoint, { method, headers: { ...headers, 'content-type': 'application/json' }, body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(180000) })
    assert.equal(response.ok, true, `${method} ${endpoint}: ${response.status}`)
    return response
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
    await writeFile(path.join(source, 'home/.codex/leo-managed-auth'), '1')
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
const root=${JSON.stringify(runRoot)};
const mode=process.argv[2];
assert.ok(Math.abs(Date.now()-Number(process.argv[3])) < 30000,'clock repaired before execution');
const project=root+'/workspace/'+${JSON.stringify(projectId)};
if(mode==='first')execFileSync('cc',['-Wall','-Wextra','-Werror','-O2',root+'/workspace/nested-kvm.c','-o',root+'/workspace/nested-kvm'],{timeout:30000});
if(mode==='first'||mode==='resume')assert.match(execFileSync(root+'/workspace/nested-kvm',[],{encoding:'utf8',timeout:15000}),/KVM_RUN, rax=42, HLT/);
assert.match(execFileSync('uname',['-r'],{encoding:'utf8'}),/^6\\.12\\.109/);
for(const path of ['/data/private-manager-canary','/data/runner-secret','/var/run/docker.sock'])assert.equal(fs.existsSync(path),false,path);
assert.equal(process.env.RUNNER_TOKEN,undefined);
if(mode==='first') {
  console.log(execFileSync('/usr/local/bin/node',[root+'/workspace/network-probe.mjs','guest',${JSON.stringify(publicPeer)},${JSON.stringify(privatePeer)}],{encoding:'utf8',timeout:15000}));
  const env=JSON.parse(execFileSync('/usr/local/bin/leo',['toolkit-env'],{encoding:'utf8'}));
  assert.equal(env.LEO_TOOLKIT_DIR,'/opt/leo-toolkit');
  for(const tool of ['cargo','rustc','pnpm','python','uv','rg','fd','gh','codex','claude'])execFileSync(tool,['--version'],{env,timeout:30000});
  const auth=await new Promise((resolve,reject)=>{const socket=net.connect('/run/leo-auth.sock',()=>socket.write('{"refresh":false}\\n'));let data='';socket.on('data',chunk=>{data+=chunk;if(data.includes('\\n')){socket.end();resolve(JSON.parse(data))}});socket.on('error',reject);});
  assert.equal(auth.accessToken,'fixture-access-token');
  assert.equal(fs.existsSync(project),false,'unopened project is absent');
  fs.writeFileSync('/tmp/artifact-probe.bin',Buffer.alloc(150000,90));
  fs.symlinkSync('/etc/passwd','/tmp/artifact-link');
  console.log('probe.ready');
  const deadline=Date.now()+20000;
  while(!fs.readFileSync('/run/leo-chat/messages.json','utf8').includes('steered')){assert.ok(Date.now()<deadline,'live inbox');await new Promise(r=>setTimeout(r,100));}
  assert.equal(fs.readFileSync(project+'/hello','utf8'),'imported on demand');
  fs.writeFileSync(project+'/hello','guest edit');
  console.log('project.edited');
  while(!fs.readFileSync('/run/leo-chat/messages.json','utf8').includes('reopened')){assert.ok(Date.now()<deadline,'reopen response');await new Promise(r=>setTimeout(r,100));}
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
  await new Promise(()=>{setInterval(()=>{},1000)});
}
if(mode==='recover')assert.equal(fs.readFileSync(root+'/workspace/interrupted','utf8'),'saved before interruption');
if(mode==='managed-claude') {
  const state=await new Promise((resolve,reject)=>{const socket=net.connect('/run/leo-auth.sock',()=>socket.write('{}\\n'));let data='';socket.on('data',chunk=>{data+=chunk;if(data.includes('\\n')){socket.end();resolve(JSON.parse(data))}});socket.on('error',reject);});
  assert.equal(state.claudeAiOauth.accessToken,'fixture-claude-access');
  assert.equal(state.claudeAiOauth.refreshToken,undefined);
  fs.writeFileSync('/home/node/.claude/.credentials.json',JSON.stringify({fixture:'must-not-overwrite-shared-state'}));
}
fs.writeFileSync(root+'/output/result.md','guest test passed '+mode);
console.log('probe.done');
`
    await writeFile(path.join(source, 'workspace/probe.mjs'), fixture)
    auth = createServer(socket => socket.once('data', () => socket.end('{"accessToken":"fixture-access-token"}\n')))
    await new Promise(resolve => auth.listen(path.join(source, 'home/.codex/leo-auth.sock'), resolve))
    claudeAuth = createServer(socket => socket.once('data', () => socket.end(`${JSON.stringify({ claudeAiOauth: { accessToken: 'fixture-claude-access', expiresAt: Date.now() + 3600000 } })}\n`)))
    await new Promise(resolve => claudeAuth.listen(path.join(source, 'home/.claude/leo-auth.sock'), resolve))
    docker('run', '-d', '--name', name, '--user', '0:0', '--read-only', '--cap-drop', 'ALL', ...['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE'].flatMap(cap => ['--cap-add', cap]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', '--device', '/dev/kvm', '--device', '/dev/net/tun', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', '6g', '--cpus', '3', '-e', 'CONCURRENCY=5', '--entrypoint', '/usr/local/bin/leo', image, 'runner-broker')
    docker('network', 'connect', '--ip', '203.0.113.2', networkName, name)
    // First prove every listener is reachable outside the guest firewall.
    process.stdout.write(docker('exec', name, '/usr/local/bin/node', `${runRoot}/workspace/network-probe.mjs`, 'control', publicPeer, privatePeer))
    url = `http://${await until(() => {
      try {
        return docker('port', name, '4311/tcp')
      }
      catch {
        return false
      }
    }, 10000)}`
    await until(() => fetch(`${url}/health`).then(r => r.ok).catch(() => false))
    await until(async () => (await (await api('/health')).json()).pool.ready === 1)
    // The prepared VM is really suspended long enough to expose guest clock drift.
    await setTimeout(31000)
    for (const mode of ['first', 'resume', 'cancel', 'crash', 'recover', 'managed-claude', 'codex-return']) {
      const id = randomUUID()
      const plan = {
        id,
        runId,
        expires: Date.now() + 300000,
        sandbox: 'yolo',
        cwd: `${runRoot}/workspace`,
        command: ['/usr/local/bin/node', `${runRoot}/workspace/probe.mjs`, mode, Date.now().toString()],
        chat: { output: `${runRoot}/output/result.md` },
        imports: [
          { source: `${runRoot}/workspace`, target: `${runRoot}/workspace`, readOnly: false },
          { source: `${runRoot}/home`, target: '/home/node', readOnly: false },
          { source: `${runRoot}/output`, target: `${runRoot}/output`, readOnly: false },
          { source: `${runRoot}/chat-input`, target: '/run/leo-chat', readOnly: true },
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
              if (text.includes('probe.pause') && mode === 'cancel') {
                const point = await (await api(`/runs/${id}/snapshot`, 'POST')).json()
                assert.ok(point.manifest.size > 0)
                const block = point.manifest.blocks.find(block => block.hash)
                assert.ok(block, 'Active capture must contain durable guest data')
                const bytes = Buffer.from(await (await api(`/snapshots/${point.id}/${block.hash}`)).arrayBuffer())
                assert.equal(createHash('sha256').update(bytes).digest('hex'), block.hash)
                await api(`/snapshots/${point.id}/discard`, 'DELETE')
                process.stdout.write(`${JSON.stringify({ mode: 'active-capture', pauseMs: point.manifest.pauseMs, indexMs: point.manifest.indexMs, status: 'passed' })}\n`)
                await api(`/runs/${id}`, 'DELETE')
              }
              if (text.includes('probe.pause') && mode === 'crash') {
                docker('kill', '--signal', 'KILL', name)
                docker('start', name)
                url = `http://${await until(() => {
                  try {
                    return docker('port', name, '4311/tcp')
                  }
                  catch {
                    return false
                  }
                })}`
                await until(() => fetch(`${url}/health`).then(r => r.ok).catch(() => false))
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
      let status = await waiting().catch((error) => {
        if (mode !== 'crash')
          throw error
        return { StatusCode: 143 }
      })
      const text = await output
      if (mode === 'crash')
        status = await waiting()
      assert.equal(status.StatusCode, ['cancel', 'crash'].includes(mode) ? 143 : 0, text)
      if (['cancel', 'crash'].includes(mode))
        assert.ok(text.includes('probe.pause'))
      else assert.equal(docker('exec', name, 'cat', `${runRoot}/output/result.md`), `guest test passed ${mode}`)
      if (mode === 'managed-claude') {
        // Guest credential writes never reach the host.
        assert.equal(JSON.parse(await readFile(path.join(source, 'home/.claude/.credentials.json'), 'utf8')).fixture, 'provisioned')
        assert.ok(!text.includes('fixture-claude-access'), 'access snapshot stays out of logs')
      }
      assert.equal(await readFile(path.join(source, 'workspace/preserved'), 'utf8').catch(() => null), null, 'guest edits must not affect host checkout')
      await api(`/runs/${id}`, 'DELETE')
      if (mode === 'first') {
        const transfer = randomUUID()
        const staging = path.join(root, 'data/archive-transfers', transfer)
        await mkdir(staging, { recursive: true, mode: 0o700 })
        await api(`/disks/${runId}/export`, 'POST', { transfer })
        assert.ok((await stat(path.join(staging, 'workspace.tar.gz'))).size > 0)
        await api(`/disks/${runId}/delete`, 'POST', {})
        docker('exec', name, 'test', '!', '-e', `/runner-state/disks/${runId}/data.ext4`)
        await api(`/disks/${runId}/import`, 'POST', { transfer })
        docker('exec', name, 'test', '-s', `/runner-state/disks/${runId}/data.ext4`)
        await rm(staging, { recursive: true })
        // The next real guest must find dirty files, session state and cached Docker images.
      }
      process.stdout.write(`${JSON.stringify({ mode, durationMs: Date.now() - start, status: 'passed' })}\n`)
    }
    // Independent disks must run concurrently and enforce each guest policy.
    const probes = []
    for (const sandbox of ['read-only', 'workspace-write']) {
      const runId = randomUUID()
      const id = randomUUID()
      const workspace = `/data/runs/${runId}/workspace`
      const lazyId = randomUUID()
      const lazy = `/data/runs/${runId}/projects/${lazyId}`
      const directory = path.join(root, 'data/runs', runId, 'workspace')
      await mkdir(directory, { recursive: true })
      await writeFile(path.join(directory, 'sentinel'), sandbox)
      const code = `
        const fs=require('node:fs'), assert=require('node:assert/strict');
        const {spawnSync}=require('node:child_process');
        assert.equal(process.getuid(),1000);
        assert.notEqual(spawnSync('sudo',['-n','true']).status,0);
        assert.equal(fs.readFileSync(${JSON.stringify(`${workspace}/sentinel`)},'utf8'),${JSON.stringify(sandbox)});
        const write=()=>fs.writeFileSync(${JSON.stringify(`${workspace}/created`)},'guest only');
        ${sandbox === 'read-only' ? 'assert.throws(write,e=>e.code===\'EROFS\');' : 'write();'}
        console.log('parallel.ready');
        const deadline=Date.now()+30000;
        const timer=setInterval(()=>{
          if(Date.now()>deadline)throw Error('lazy policy import timeout');
          if(!fs.existsSync(${JSON.stringify(`${lazy}/sentinel`)}))return;
          clearInterval(timer);
          assert.equal(fs.readFileSync(${JSON.stringify(`${lazy}/sentinel`)},'utf8'),'lazy');
          const write=()=>fs.writeFileSync(${JSON.stringify(`${lazy}/changed`)},'guest');
          ${sandbox === 'read-only' ? 'assert.throws(write,e=>e.code===\'EROFS\');' : 'write();'}
          console.log('parallel.done');
        },100);

      `
      const plan = { id, runId, expires: Date.now() + 60000, sandbox, cwd: workspace, command: ['/usr/local/bin/node', '-e', code], imports: [{ source: workspace, target: workspace, readOnly: sandbox === 'read-only' }] }
      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      await api(`/runs/${id}`, 'POST')
      probes.push({ id, runId, sandbox, directory, lazyId, lazy })
    }
    const outcomes = await Promise.allSettled(probes.map(async ({ id, runId, sandbox, directory, lazyId, lazy }) => {
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
    const resumed = { ...previous, id: resumedId, expires: Date.now() + 60000, command: ['/usr/local/bin/node', '-e', code] }
    await writeFile(path.join(root, 'data/runner-plans', `${resumedId}.json`), JSON.stringify(resumed))
    await api(`/runs/${resumedId}`, 'POST')
    const status = await (await api(`/runs/${resumedId}/wait`, 'POST')).json()
    assert.equal(status.StatusCode, 0, 'lazy read-only policy survives VM restart')
    process.stdout.write(`${JSON.stringify({ mode: 'read-only-resume', status: 'passed' })}\n`)
    // Prepared VMs share the configured five slots with active work. A sixth run cannot enter.
    await until(async () => {
      const health = await (await api('/health')).json()
      return health.activeRuns === 0 && health.pool.ready === 1
    })
    // Kill only this disposable controller's anonymous spare. Admission must fall
    // back before executing the user command, without leaking the occupied slot.
    docker('exec', name, 'pkill', '-KILL', '-x', 'firecracker')
    const held = []
    for (let index = 0; index < 6; index++) {
      const id = randomUUID()
      const runId = randomUUID()
      const cwd = `/data/runs/${runId}/workspace`
      const directory = path.join(root, 'data/runs', runId, 'workspace')
      await mkdir(directory, { recursive: true })
      const plan = { id, runId, expires: Date.now() + 60000, sandbox: 'yolo', cwd, command: ['/usr/local/bin/node', '-e', 'console.log("slot.ready");setInterval(()=>{},1000)'], imports: [{ source: cwd, target: cwd }] }
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
      try {
        return held.every((id) => {
          const logs = docker('exec', name, 'cat', `/runner-state/${id}.log`)
          return logs.split('\n').filter(Boolean).some(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString().includes('slot.ready'))
        })
      }
      catch {
        return false
      }
    })
    await Promise.all(held.map(id => api(`/runs/${id}`, 'DELETE')))
    await until(async () => {
      const health = await (await api('/health')).json()
      return health.activeRuns === 0 && health.pool.ready === 1 && health.pool.occupied === 1
    })
    process.stdout.write(`${JSON.stringify({ mode: 'pool-capacity-cancel-refill', capacity: 5, status: 'passed' })}\n`)
  }
  catch (error) {
    console.error(error)
    try {
      console.error(docker('logs', name))
      console.error(docker('exec', name, 'sh', '-c', 'tail -60 /runner-state/*.boot.log'))
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
    }
    if (auth)
      await new Promise(resolve => auth.close(resolve))
    if (claudeAuth)
      await new Promise(resolve => claudeAuth.close(resolve))
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
