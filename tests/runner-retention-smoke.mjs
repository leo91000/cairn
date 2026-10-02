// Real native Codex in Firecracker, with synthetic account/model/MCP access.
// Run twice with LEO_VM_RETENTION_SECONDS=0/180 to compare cold and retained resumes.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync, spawnSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import { createServer } from 'node:net'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { stripVTControlCharacters } from 'node:util'
import { blockDigest } from './block-digest.mjs'
import { prepareStorageOrigin } from './runner-storage-smoke.mjs'

const peerCode = `
const http=require('node:http'),crypto=require('node:crypto');
const requests=[];
http.createServer(async(req,res)=>{
  if(req.url==='/records'){res.setHeader('content-type','application/json');res.end(JSON.stringify(requests));return;}
  if(req.method==='GET'){res.setHeader('content-type','application/json');res.end('{}');return;}
  let raw='';for await(const chunk of req)raw+=chunk;const body=raw?JSON.parse(raw):{};
  if(req.url==='/mcp'){
    if(req.method!=='POST'){res.writeHead(405).end();return;}
    const lease=req.headers.authorization;
    if(!['Bearer alpha','Bearer beta'].includes(lease)){res.writeHead(403).end();return;}
    if(body.id===undefined){res.writeHead(202).end();return;}
    const result=body.method==='initialize'?{protocolVersion:body.params.protocolVersion,capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}}:
      body.method==='tools/list'?{tools:[{name:lease==='Bearer alpha'?'alpha_tool':'beta_tool',description:'Synthetic lease',inputSchema:{type:'object',properties:{}}}]}:{content:[]};
    res.setHeader('content-type','application/json');res.end(JSON.stringify({jsonrpc:'2.0',id:body.id,result}));return;
  }
  if(!req.url.endsWith('/responses')||req.method!=='POST'){res.writeHead(404).end('{}');return;}
  const tools=JSON.stringify(body.tools),input=JSON.stringify(body.input);
  requests.push({at:Date.now(),tools:body.tools?.length?['alpha_tool','beta_tool'].filter(name=>tools.includes(name)).join(',')||'no-mcp':null,labels:[...new Set(input.match(/lease-\\d+/g)||[])]});
  await new Promise(resolve=>setTimeout(resolve,1500));
  const item={id:'msg_'+crypto.randomUUID(),type:'message',role:'assistant',status:'completed',content:[{type:'output_text',text:'RETENTION_NATIVE_OK',annotations:[]}]};
  const response={id:'resp_'+crypto.randomUUID(),object:'response',status:'completed',output:[item],usage:{input_tokens:10,output_tokens:3,total_tokens:13,input_tokens_details:{cached_tokens:0},output_tokens_details:{reasoning_tokens:0}}};
  res.setHeader('content-type','text/event-stream');
  for(const event of [
    {type:'response.created',response:{...response,status:'in_progress',output:[]}},
    {type:'response.output_item.added',output_index:0,item:{...item,status:'in_progress',content:[]}},
    {type:'response.output_text.delta',item_id:item.id,output_index:0,content_index:0,delta:'RETENTION_NATIVE_OK'},
    {type:'response.output_item.done',output_index:0,item},
    {type:'response.completed',response},
  ])res.write('event: '+event.type+'\\ndata: '+JSON.stringify(event)+'\\n\\n');res.end();
}).listen(8080,'0.0.0.0');
`

async function main() {
  const image = process.argv[2]
  assert.ok(image, 'Provide the runner image')
  const retention = Number(process.env.LEO_VM_RETENTION_SECONDS ?? 180)
  const cpuQuota = process.env.LEO_RETENTION_CPU_QUOTA ?? '3'
  const docker = (...args) => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 180000 }).trim()
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || '/var/tmp', 'leo-retention-'))
  const name = `leo-retention-${randomUUID().slice(0, 8)}`
  const peer = `${name}-peer`
  const network = `${name}-public`
  const endpoint = 'http://203.0.113.3:8080'
  const runId = randomUUID()
  const runRoot = `/data/runs/${runId}`
  const samples = []
  const uploaded = new Set()
  let publications = 0
  const benchmarkStarted = Date.now()
  let broker
  let credentials = 0
  let url
  let currentLease = 0

  async function until(operation, timeout = 120000) {
    const deadline = Date.now() + timeout
    while (Date.now() < deadline) {
      const result = await operation()
      if (result)
        return result
      await setTimeout(50)
    }

    throw new Error('Retention qualification timed out')
  }

  async function api(endpoint, method = 'GET', body) {
    const response = await fetch(url + endpoint, {
      method,
      headers: { 'authorization': 'Bearer fixture-runner-token', 'content-type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(120000),
    })
    assert.ok(response.ok, `${method} ${endpoint}: ${response.status}`)
    return response
  }

  function runnerLogs(tail) {
    const result = spawnSync('docker', ['--context', 'default', 'logs', ...(tail ? ['--tail', tail] : []), name], { encoding: 'utf8', timeout: 10000, maxBuffer: 32 * 1024 * 1024 })
    assert.equal(result.status, 0)
    return result.stdout + result.stderr
  }

  function records() {
    return JSON.parse(docker('exec', peer, 'node', '-e', 'fetch(\'http://127.0.0.1:8080/records\').then(r=>r.json()).then(v=>console.log(JSON.stringify(v)))'))
  }

  function physical(vmId) {
    const script = `(function(){const fs=require('node:fs');const id=process.argv[1];const shmem=Number(fs.readFileSync('/sys/fs/cgroup/memory.stat','utf8').match(/^shmem (\\d+)/m)[1]);const files=new Set();let allocatedShared=0;for(const fd of fs.readdirSync('/proc/1/fd')){try{const path='/proc/1/fd/'+fd;if(!fs.readlinkSync(path).includes('memfd:'))continue;const metadata=fs.statSync(path);const key=metadata.dev+':'+metadata.ino;if(files.has(key))continue;files.add(key);allocatedShared+=metadata.blocks*512;}catch{}}for(const p of fs.readdirSync('/proc')){if(!/^\\d+$/.test(p))continue;try{const cmd=fs.readFileSync('/proc/'+p+'/cmdline','utf8');if(!cmd.includes(id)||fs.readFileSync('/proc/'+p+'/comm','utf8').trim()!=='firecracker')continue;const status=fs.readFileSync('/proc/'+p+'/status','utf8');const rss=Number(status.match(/VmRSS:\\s+(\\d+)/)[1])*1024;const mapped=Number(status.match(/RssShmem:\\s+(\\d+)/)[1])*1024;console.log(JSON.stringify({pid:Number(p),rssBytes:rss,mappedSharedBytes:mapped,cgroupShmemBytes:shmem,cgroupMemoryBytes:Number(fs.readFileSync('/sys/fs/cgroup/memory.current','utf8')),allocatedSharedGuestBytes:allocatedShared,conservativeVmBytes:rss+Math.max(0,allocatedShared-mapped)}));return;}catch{}}throw Error('Owned VMM not found');})()`
    const sample = JSON.parse(docker('exec', name, 'node', '-e', script, vmId))
    assert.ok(sample.allocatedSharedGuestBytes > 0, 'The observer can inspect the registered guest memfd')
    return sample
  }

  function state(vmId) {
    const script = `const http=require('node:http');http.get({socketPath:process.argv[1],path:'/'},res=>{let body='';res.on('data',v=>body+=v);res.on('end',()=>console.log(body));}).on('error',()=>process.exit(1));`
    return JSON.parse(docker('exec', name, 'node', '-e', script, `/runner-state/jails/firecracker/${vmId}/root/api.sock`)).state
  }

  try {
    for (const directory of ['data/runner-plans', 'state', `data/runs/${runId}/workspace`, `data/runs/${runId}/home/.codex`, `data/runs/${runId}/chat-input`, `data/runs/${runId}/output`])
      await mkdir(path.join(root, directory), { recursive: true })
    await writeFile(path.join(root, 'data/runner-secret'), 'fixture-runner-token')
    await writeFile(path.join(root, 'peer.cjs'), peerCode)
    const home = path.join(root, 'data/runs', runId, 'home/.codex')
    await writeFile(path.join(home, 'config.toml'), `cli_auth_credentials_store = "file"\nchatgpt_base_url = "${endpoint}"\nmodel_provider = "fixture"\n[model_providers.fixture]\nname = "Fixture"\nbase_url = "${endpoint}"\nwire_api = "responses"\nrequires_openai_auth = false\n`)
    await writeFile(path.join(home, 'leo-managed-auth'), '1')
    broker = createServer((client) => {
      let input = ''
      client.on('data', (bytes) => {
        input += bytes
        if (!input.includes('\n'))
          return
        JSON.parse(input.trim())
        credentials++
        const claims = Buffer.from(JSON.stringify({ 'sub': 'synthetic-retention', 'exp': Math.floor(Date.now() / 1000) + 3600, 'https://api.openai.com/auth': { chatgpt_account_id: 'synthetic-retention', chatgpt_plan_type: 'plus' } })).toString('base64url')
        client.end(`${JSON.stringify({ accessToken: `eyJhbGciOiJub25lIn0.${claims}.synthetic-${currentLease}`, chatgptAccountId: 'synthetic-retention', chatgptPlanType: 'plus' })}\n`)
      })
    })
    await new Promise(resolve => broker.listen(path.join(home, 'leo-auth.sock'), resolve))
    docker('network', 'create', '--internal', '--subnet', '203.0.113.0/29', network)
    docker('run', '-d', '--name', peer, '-v', `${root}/peer.cjs:/peer.cjs:ro`, '--entrypoint', 'node', image, '/peer.cjs')
    docker('network', 'connect', '--ip', '203.0.113.3', network, peer)
    const capabilities = ['SYS_ADMIN', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE']
    docker('run', '-d', '--name', name, '--user', '0:0', '--read-only', '--cap-drop', 'ALL', ...capabilities.flatMap(value => ['--cap-add', value]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', '--device', '/dev/kvm', '--device', '/dev/fuse', '--device', '/dev/net/tun', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', `${process.env.LEO_RETENTION_MEMORY_GIB ?? 10}g`, '--cpus', cpuQuota, '-e', 'CONCURRENCY=3', '-e', `LEO_VM_RETENTION_SECONDS=${retention}`, '-e', 'LEO_READY_VM_POOL=false', '--entrypoint', '/usr/local/bin/leo', image, 'runner-broker')
    docker('network', 'connect', '--ip', '203.0.113.2', network, name)
    url = `http://${await until(() => {
      try {
        return docker('port', name, '4311/tcp')
      }
      catch {
        return false
      }
    }, 10000)}`
    await until(() => fetch(`${url}/health`).then(r => r.ok).catch(() => false))
    const { storage, origin } = await prepareStorageOrigin({
      root,
      docker,
      name,
      api,
    })
    let sessionId
    let previousVm
    let evictedVm
    let crashRecoveryPassed = false
    let expiryPassed = false
    for (let lease = 0; lease < Number(process.env.LEO_RETENTION_TURNS ?? 3); lease++) {
      currentLease = lease
      const id = randomUUID()
      const bearer = lease === 0 ? 'Bearer alpha' : lease === 1 ? 'Bearer beta' : null
      const chat = {
        runId,
        attemptId: id,
        provider: 'codex',
        execution: { messageId: randomUUID(), text: `Remember lease-${lease}; reply RETENTION_NATIVE_OK.`, recovery: false },
        instructions: 'Reply with the requested marker.',
        inputDirectory: '/run/leo-chat',
        output: `${runRoot}/output/result.md`,
        cwd: `${runRoot}/workspace`,
        model: 'gpt-5.4',
        reasoning: 'low',
        sandbox: 'yolo',
        writableRoots: [`${runRoot}/workspace`],
        args: [
          '-c',
          'model_provider="fixture"',
          '-c',
          `model_providers.fixture={name="Fixture",base_url="${endpoint}",wire_api="responses",requires_openai_auth=false}`,
          ...(bearer ? ['-c', `mcp_servers.fixture={url="${endpoint}/mcp",http_headers={Authorization="${bearer}"}}`] : []),
        ],
        codexConfig: {
          chatgpt_base_url: endpoint,
          model_provider: 'fixture',
          model_providers: {
            fixture: {
              name: 'Fixture',
              base_url: endpoint,
              wire_api: 'responses',
              requires_openai_auth: false,
            },
          },
          mcp_servers: bearer ? { fixture: { url: `${endpoint}/mcp`, http_headers: { Authorization: bearer } } } : {},
        },
        ...(sessionId ? { sessionId } : {}),
      }
      const plan = {
        id,
        runId,
        storage,
        expires: Date.now() + 120000,
        sandbox: 'yolo',
        cwd: chat.cwd,
        resources: { cpu: 3, memoryMiB: 1024, diskMiB: 32768 },
        chat,
        imports: [{ source: `${runRoot}/home`, target: '/home/node' }, { source: `${runRoot}/workspace`, target: chat.cwd }, { source: `${runRoot}/output`, target: `${runRoot}/output` }, { source: `${runRoot}/chat-input`, target: '/run/leo-chat' }],
      }
      await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
      const before = records().length
      const started = Date.now()
      await api(`/runs/${id}`, 'POST')
      const request = await until(async () => {
        const request = records().slice(before).find(value => value.tools)
        if (request)
          return request
        try {
          const exit = docker('exec', name, 'node', '-e', `const fs=require('node:fs');const p=process.argv[1];console.log(fs.existsSync(p)?fs.readFileSync(p,'utf8'):'pending');`, `/runner-state/${id}.exit`)
          if (exit === 'pending')
            return false
          assert.equal(Number(exit), 0, 'Native turn failed before model request')
        }
        catch (error) {
          if (error.code !== 'ENOENT')
            throw error
        }

        return false
      })
      const { vmId } = JSON.parse(docker('exec', name, 'cat', `/runner-state/${id}.vm.json`))
      const active = physical(vmId)
      const exit = await (await api(`/runs/${id}/wait`, 'POST')).json()
      assert.equal(exit.StatusCode, 0)
      const finished = Date.now()
      const output = docker('exec', name, 'cat', `/runner-state/${id}.log`).trim().split('\n').map(line => JSON.parse(line)).filter(event => event.type === 'output').map(event => Buffer.from(event.data, 'base64').toString()).join('')
      const logs = output.split('\n').filter(Boolean).flatMap((line) => {
        try {
          return [JSON.parse(line)]
        }
        catch {
          return []
        }
      })
      sessionId = logs.find(value => value.type === 'thread.started')?.thread_id
      assert.ok(sessionId)
      assert.match(docker('exec', name, 'cat', `${runRoot}/output/result.md`), /RETENTION_NATIVE_OK/)
      if (lease > 0)
        assert.ok(request.labels.includes(`lease-${lease - 1}`), 'Native restores the unique previous-turn context')
      assert.equal(request.tools.includes('alpha_tool'), lease === 0)
      assert.equal(request.tools.includes('beta_tool'), lease === 1)
      const health = await (await api('/health')).json()
      let retained
      if (retention > 0) {
        assert.equal(health.pool.retained, 1)
        assert.equal(state(vmId), 'Paused')
        retained = physical(vmId)
        assert.ok(Math.abs(health.pool.retained_memory_mi_b * 1048576 - retained.conservativeVmBytes) < 32 * 1048576, `Admission agrees with independently sampled backing inodes: ${JSON.stringify({ costMiB: health.pool.retained_memory_mi_b, retained })}`)
        if (previousVm)
          assert.equal(vmId, previousVm, 'Resume keeps the same physical VMM')
        if (evictedVm) {
          assert.notEqual(vmId, evictedVm, 'Evicted conversations resume from their durable disk')
          crashRecoveryPassed ||= process.env.LEO_RETENTION_CRASH_AFTER !== undefined
          evictedVm = undefined
        }

        for (let index = 0; index < 2; index++) {
          const snapshot = await (await api(`/disks/${runId}/snapshot`, 'POST', {})).json()
          assert.ok(snapshot.manifest.generation > 0)
          assert.equal(snapshot.manifest.consistency, 'crash', 'CPU pause does not promise filesystem freeze')
          assert.equal(state(vmId), 'Paused', 'Publication never wakes retained CPUs')
          if (process.env.LEO_RETENTION_PUBLISH === 'true' && index === 0 && lease % 10 === 0) {
            for (const block of snapshot.manifest.blocks) {
              if (!block.hash || uploaded.has(block.hash))
                continue
              const bytes = Buffer.from(await (await api(`/snapshots/${snapshot.id}/${block.hash}`)).arrayBuffer())
              assert.equal(blockDigest(bytes, block.hash), block.hash)
              await writeFile(path.join(origin, block.hash), bytes)
              uploaded.add(block.hash)
            }

            const backupId = randomUUID()
            await api(`/disks/${runId}/published`, 'POST', { generation: snapshot.manifest.generation, grantId: snapshot.grantId, backupId })
            const status = await (await api(`/disks/${runId}/storage-status`, 'POST', {})).json()
            assert.equal(status.published.backupId, backupId)
            assert.equal(state(vmId), 'Paused', 'Publication acknowledgement also leaves CPUs paused')
            publications++
          }

          await api(`/snapshots/${snapshot.id}/discard`, 'DELETE')
        }
      }
      else {
        assert.equal(health.pool.retained, 0)
        if (previousVm)
          assert.notEqual(vmId, previousVm)
      }

      samples.push({
        lease,
        vmId,
        preModelMs: request.at - started,
        nativeInitMs: Number(stripVTControlCharacters(output).match(/method="initialize"[^\n]*event="completed"[^\n]*elapsed_ms=(\d+)/)?.[1]),
        totalMs: finished - started,
        active,
        retained,
        health: { pool: health.pool, usage: health.usage },
      })
      previousVm = vmId
      if (retention > 0 && lease === Number(process.env.LEO_RETENTION_CRASH_AFTER ?? -1)) {
        docker('exec', name, 'node', '-e', 'process.kill(Number(process.argv[1]), "SIGKILL")', retained.pid.toString())
        await until(async () => (await (await api('/health')).json()).pool.retained === 0)
        previousVm = undefined
        evictedVm = vmId
      }

      if (retention > 0 && lease === Number(process.env.LEO_RETENTION_EXPIRE_AFTER ?? -1)) {
        await until(async () => (await (await api('/health')).json()).pool.retained === 0, (retention + 5) * 1000)
        previousVm = undefined
        evictedVm = vmId
        expiryPassed = true
      }
    }

    assert.ok(credentials >= samples.length, 'Each turn renews account access')
    let activeAdmissionPassed = false
    if (retention > 0 && process.env.LEO_RETENTION_ADMISSION === 'true') {
      const active = []
      const occupied = (await (await api('/health')).json()).pool.occupied
      for (let index = occupied; index <= 3; index++) {
        const id = randomUUID()
        const taskRun = randomUUID()
        const cwd = `/data/runs/${taskRun}/workspace`
        const plan = {
          id,
          runId: taskRun,
          storage,
          expires: Date.now() + 120000,
          sandbox: 'yolo',
          cwd,
          resources: { cpu: 1, memoryMiB: 512, diskMiB: 512 },
          imports: [],
          command: ['/bin/sh', '-c', 'echo ACTIVE_TASK_OK; sleep 60'],
        }
        await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
        await api(`/runs/${id}`, 'POST')
        active.push({ id, run: taskRun })
        await until(() => docker('exec', name, 'node', '-e', `console.log(require('node:fs').existsSync('/runner-state/${id}.vm.json') ? 'yes' : 'no')`) === 'yes')
      }

      const health = await (await api('/health')).json()
      assert.equal(health.pool.retained, 0, 'Active admission evicts the retained guest')
      assert.equal(health.pool.occupied, 3)
      for (const task of active) {
        await api(`/runs/${task.id}`, 'DELETE')
        await api(`/runs/${task.id}/wait`, 'POST')
        await api(`/disks/${task.run}/delete`, 'POST', {})
      }

      activeAdmissionPassed = true
    }

    await api(`/disks/${runId}/delete`, 'POST', {})
    await until(async () => (await (await api('/health')).json()).pool.occupied === 0)
    const runnerLog = stripVTControlCharacters(runnerLogs())
    const reclamation = runnerLog.split('\n').filter(line => line.includes('event="memory_reclaimed"')).map((line) => {
      const value = field => Number(line.match(new RegExp(`${field}=(\\d+)`))?.[1])
      return {
        activeBytes: value('active_bytes'),
        retainedBytes: value('retained_bytes'),
        reclaimedBytes: value('reclaimed_bytes'),
        balloonMiB: value('balloon_mib'),
        elapsedMs: value('elapsed_ms'),
      }
    })
    const evidence = {
      kind: 'real-native-vm-retention',
      image,
      cpuQuota,
      retentionSeconds: retention,
      scope: 'Local real Firecracker/native Codex, synthetic account/model/MCP and loopback block origin. Model response deliberately delayed 1.5 s; preModel excludes that delay. No manager, production or WAN.',
      durationMs: Date.now() - benchmarkStarted,
      publications,
      samples,
      reclamation,
      crashRecoveryPassed,
      expiryPassed,
      activeAdmissionPassed,
      renewedAccountLeases: credentials,
      mcpLeasesRenewedAndRemoved: true,
      contextPreserved: true,
      pausedCapturePassed: retention > 0,
      deletionReaped: true,
    }
    if (process.env.LEO_RETENTION_EVIDENCE)
      await writeFile(process.env.LEO_RETENTION_EVIDENCE, JSON.stringify(evidence, null, 2))
    process.stdout.write(`${JSON.stringify(evidence)}\n`)
  }
  catch (error) {
    try {
      const logs = runnerLogs('100')
      if (process.env.LEO_RETENTION_EVIDENCE) {
        await writeFile(`${process.env.LEO_RETENTION_EVIDENCE}.failure.log`, logs)
        const files = JSON.parse(docker('exec', name, 'node', '-e', `const fs=require('node:fs');console.log(JSON.stringify(fs.readdirSync('/runner-state').filter(p=>p.endsWith('.log')||p.includes('exit')).map(p=>({file:p,data:fs.readFileSync('/runner-state/'+p,'utf8')}))));`))
        await writeFile(`${process.env.LEO_RETENTION_EVIDENCE}.attempts.json`, JSON.stringify(files, null, 2))
      }
    }
    catch {}

    throw error
  }
  finally {
    broker?.close()
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

    docker('run', '--rm', '--user', '0:0', '-v', `${root}:/fixture`, '--entrypoint', '/bin/rm', image, '-rf', '/fixture/data', '/fixture/state')
    await rm(root, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
