// Real Firecracker, retained commands, nested KVM, paging and admission fencing.
// Run on a disposable host with compressed or encrypted swap for RAM qualification.
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { execFileSync, spawnSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  copyFile,
  mkdir,
  mkdtemp,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { prepareStorageOrigin } from './runner-storage-smoke.mjs'
import { ublkDeviceArguments } from './ublk-devices.mjs'

async function main() {
  const image = process.argv[2]
  assert.ok(image, 'Provide the exact runner image')
  const docker = (...args) => execFileSync('docker', ['--context', 'default', ...args], { encoding: 'utf8', timeout: 180000 }).trim()
  const root = await mkdtemp(path.join(process.env.VM_TEST_ROOT || '/var/tmp', 'cairn-exclusive-'))
  const name = `cairn-exclusive-${randomUUID().slice(0, 8)}`
  const memoryRequired = process.env.CAIRN_EXCLUSIVE_REQUIRE_RECLAIM === 'true'
  const android = process.env.CAIRN_EXCLUSIVE_ANDROID === 'true'
  const envelopeMiB = android ? 12288 : 4096
  let url
  const entries = []

  async function until(operation, timeout = 180000) {
    const deadline = Date.now() + timeout
    while (Date.now() < deadline) {
      const result = await operation()
      if (result)
        return result
      await setTimeout(100)
    }

    throw new Error('Exclusive VM qualification timed out')
  }

  async function request(route, method = 'GET', body) {
    return fetch(url + route, {
      method,
      headers: { 'authorization': 'Bearer fixture-runner-token', 'content-type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(180000),
    })
  }

  async function api(route, method = 'GET', body) {
    const response = await request(route, method, body)
    assert.ok(response.ok, `${method} ${route}: ${response.status} ${response.ok ? '' : await response.text()}`)
    return response
  }

  function logs(entry) {
    try {
      return docker('exec', name, 'cat', `/runner-state/${entry.id}.log`).split('\n').filter(Boolean).map(line => Buffer.from(JSON.parse(line).data || '', 'base64').toString()).join('')
    }
    catch { return '' }
  }

  function snapshot(entry) {
    const vm = JSON.parse(docker('exec', name, 'cat', `/runner-state/${entry.id}.vm.json`)).vmId
    const script = `
const fs=require('node:fs');
const vm=${JSON.stringify(vm)};
const expected=fs.statSync('/runner-state/jails/firecracker/'+vm+'/root');
for(const pid of fs.readdirSync('/proc').filter(v=>/^\\d+$/.test(v))){
  try{
    const actual=fs.statSync('/proc/'+pid+'/root');
    if(actual.ino!==expected.ino||actual.dev!==expected.dev)continue;
    const status=fs.readFileSync('/proc/'+pid+'/status','utf8');
    const field=name=>Number(status.match(new RegExp('^'+name+':\\\\s+(\\\\d+)','m'))?.[1]||0)*1024;
    console.log(JSON.stringify({vm,pid:Number(pid),resident:field('VmRSS'),swap:field('VmSwap')}));break;
  }catch{}
}`
    return JSON.parse(docker('exec', name, 'node', '-e', script))
  }

  function hostAvailableMemoryMiB() {
    const memory = docker('exec', name, 'cat', '/proc/meminfo').match(/^MemAvailable:\s+(\d+)\s+kB$/m)
    assert.ok(memory, 'Host available memory can be measured')
    return Math.floor(Number(memory[1]) / 1024)
  }

  async function makeEntry(storage, mode) {
    const runId = randomUUID()
    const id = randomUUID()
    const runRoot = `/data/runs/${runId}`
    const source = path.join(root, 'data/runs', runId)
    for (const directory of ['workspace', 'home', 'output', 'chat-input'])
      await mkdir(path.join(source, directory), { recursive: true })
    await writeFile(path.join(source, 'chat-input/messages.json'), '[]')
    await copyFile(new URL('./fixtures/nested-kvm-resume.c', import.meta.url), path.join(source, 'workspace/nested-kvm-resume.c'))
    await copyFile(new URL('./fixtures/exclusive-inbox.mjs', import.meta.url), path.join(source, 'workspace/exclusive-inbox.mjs'))
    const fixture = `
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {createHash} from 'node:crypto';
import {execFileSync,spawn} from 'node:child_process';
import {setTimeout as delay} from 'node:timers/promises';
import {finishRequested} from './exclusive-inbox.mjs';
const root=${JSON.stringify(runRoot)},mode=${JSON.stringify(mode)},android=${android && mode === 'victim'};
fs.appendFileSync(root+'/workspace/invocations',process.pid+'\\n');
const bytes=Buffer.alloc(mode==='victim'?384*1024**2:16*1024**2,0x5a);
for(let i=0;i<bytes.length;i+=4096)bytes[i]=(i/4096)%251;
const digest=()=>createHash('sha256').update(bytes).digest('hex');
const original=digest();
let nested;
let androidBoot;
let env;
if(mode==='victim'){
  execFileSync('cc',['-Wall','-Wextra','-Werror','-O2',root+'/workspace/nested-kvm-resume.c','-o',root+'/workspace/nested-live'],{timeout:30000});
  nested=spawn(root+'/workspace/nested-live',[],{stdio:['ignore','pipe','pipe']});
  nested.stdout.pipe(process.stdout);nested.stderr.pipe(process.stderr);
}
if(android){
  env=JSON.parse(execFileSync('/usr/local/bin/cairn',['toolkit-env'],{encoding:'utf8'}));
  execFileSync('cairn-android',['emulator','start','34','--aosp','--accept-licenses'],{env,timeout:1200000,stdio:['ignore','pipe','pipe']});
  const state=JSON.parse(execFileSync('cairn-android',['emulator','status'],{env,encoding:'utf8'}));
  assert.equal(state.state,'ready');assert.equal(state.acceleration,'kvm');
  androidBoot=execFileSync('adb',['-s','emulator-5580','shell','cat','/proc/sys/kernel/random/boot_id'],{env,encoding:'utf8'}).trim();
}
console.log('ready '+mode+' pid='+process.pid+' memory='+original+(android?' android='+androidBoot:''));
let ticks=0;
while(!finishRequested('/run/cairn-chat/messages.json')){
  console.log('tick '+mode+' pid='+process.pid+' count='+(++ticks));
  await delay(100);
}
assert.equal(digest(),original,'live process retains every guest memory page');
assert.equal(fs.readFileSync(root+'/workspace/invocations','utf8').trim(),String(process.pid),'command executes once');
if(android){
  assert.equal(execFileSync('adb',['-s','emulator-5580','shell','cat','/proc/sys/kernel/random/boot_id'],{env,encoding:'utf8'}).trim(),androidBoot,'same Android emulator remains booted');
  assert.equal(execFileSync('adb',['-s','emulator-5580','shell','getprop','sys.boot_completed'],{env,encoding:'utf8'}).trim(),'1');
  execFileSync('cairn-android',['emulator','stop'],{env,timeout:120000});
}
if(nested){nested.kill('SIGTERM');await new Promise(resolve=>nested.once('exit',resolve));}
console.log('exclusive-live-ok '+mode+' pid='+process.pid);
`
    await writeFile(path.join(source, 'workspace/probe.mjs'), fixture)
    const plan = {
      id,
      runId,
      expires: Date.now() + (android ? 1800000 : 300000),
      sandbox: 'yolo',
      cwd: `${runRoot}/workspace`,
      storage,
      resources: { cpu: 2, memoryMiB: envelopeMiB, diskMiB: 32768 },
      command: ['node', `${runRoot}/workspace/probe.mjs`],
      chat: { provider: 'codex' },
      imports: [
        { source: `${runRoot}/workspace`, target: `${runRoot}/workspace` },
        { source: `${runRoot}/home`, target: '/home/node' },
        { source: `${runRoot}/chat-input`, target: '/run/cairn-chat', readOnly: true },
      ],
    }
    await writeFile(path.join(root, 'data/runner-plans', `${id}.json`), JSON.stringify(plan))
    const entry = {
      id,
      runId,
      source,
      mode,
    }
    entries.push(entry)
    return entry
  }

  try {
    for (const directory of ['data/runner-plans', 'state'])
      await mkdir(path.join(root, directory), { recursive: true })
    await writeFile(path.join(root, 'data/runner-secret'), 'fixture-runner-token')
    const capabilities = ['SYS_ADMIN', 'SYS_PTRACE', 'SYS_NICE', 'NET_ADMIN', 'SYS_CHROOT', 'SETUID', 'SETGID', 'MKNOD', 'CHOWN', 'FOWNER', 'KILL', 'DAC_OVERRIDE']
    docker('run', '-d', '--name', name, '--user', '0:0', '--read-only', '--cap-drop', 'ALL', ...capabilities.flatMap(cap => ['--cap-add', cap]), '--security-opt', 'apparmor=unconfined', '--security-opt', 'seccomp=unconfined', ...ublkDeviceArguments(), '--device', '/dev/kvm', '--device', '/dev/net/tun', '--sysctl', 'net.ipv4.ip_forward=1', '--sysctl', 'net.ipv6.conf.all.disable_ipv6=1', '--tmpfs', '/run', '--tmpfs', '/tmp', '-v', `${root}/data:/data`, '-v', `${root}/state:/runner-state`, '-p', '127.0.0.1::4311', '--memory', `${envelopeMiB}m`, '--cpus', '3', '-e', 'CONCURRENCY=4', '-e', 'CAIRN_READY_VM_POOL=false', '-e', 'RUST_LOG=warn,cairn_performance=info', '--entrypoint', '/usr/local/bin/cairn', image, 'runner-broker')
    url = `http://${docker('port', name, '4311/tcp')}`
    await until(() => fetch(`${url}/health`).then(response => response.ok).catch(() => false))
    const fixture = await prepareStorageOrigin({
      root,
      docker,
      name,
      api,
      maxDirtySeconds: 3600,
    })
    const owner = await makeEntry(fixture.storage, 'owner')
    const victim = await makeEntry(fixture.storage, 'victim')
    const waiting = await makeEntry(fixture.storage, 'waiting')
    await Promise.all([owner, victim].map(entry => api(`/runs/${entry.id}`, 'POST')))
    await until(() => logs(owner).includes('ready owner') && logs(victim).includes('ready victim'), android ? 1500000 : 180000)
    await until(() => /nested-live pid=\d+ value=\d+/.test(logs(victim)))
    const before = snapshot(victim)
    const guestPid = logs(victim).match(/ready victim pid=(\d+)/)[1]
    const nestedPid = logs(victim).match(/nested-live pid=(\d+)/)[1]
    const nestedProgress = () => {
      const progress = [...logs(victim).matchAll(/nested-live pid=(\d+) value=(\d+)/g)].at(-1)
      assert.ok(progress, 'The nested vCPU emits a progress counter')
      assert.equal(progress[1], nestedPid, 'The same nested process remains active')
      return BigInt(progress[2])
    }

    const requireNestedProgress = previous => until(() => nestedProgress() > previous)
    let epoch = 1
    const reservation = () => ({
      id: randomUUID(),
      epoch: epoch++,
      ownerAttemptId: owner.id,
      memberAttemptIds: [],
      expiresAt: Date.now() + 120000,
      reason: 'Disposable live VM qualification',
    })
    const held = reservation()
    const acquired = await (await api('/exclusive/acquire', 'POST', held)).json()
    assert.equal(acquired.reservation.state, 'acquired')
    assert.deepEqual(acquired.reservation.pausedAttemptIds, [victim.id])
    await setTimeout(300)
    const paused = logs(victim)
    const ownerTicks = (logs(owner).match(/tick owner/g) || []).length
    await setTimeout(1200)
    assert.equal(logs(victim), paused, 'Paused command and nested VM stop producing events')
    assert.ok((logs(owner).match(/tick owner/g) || []).length > ownerTicks, 'Exclusive owner continues running')
    const denied = await request(`/runs/${waiting.id}`, 'POST')
    assert.equal(denied.status, 409, 'New admissions wait behind the exclusive reservation')
    const checkpoint = await (await api(`/runs/${victim.id}/snapshot`, 'POST')).json()
    await api(`/snapshots/${checkpoint.id}/discard`, 'DELETE')
    await setTimeout(300)
    assert.equal(logs(victim), paused, 'Checkpoint completion does not release exclusivity')
    const pausedNestedCounter = nestedProgress()
    await api('/exclusive/release', 'POST', { id: held.id, epoch: held.epoch })
    await requireNestedProgress(pausedNestedCounter)
    assert.equal(snapshot(victim).pid, before.pid, 'The same live VMM resumes')
    assert.ok(logs(victim).includes(`tick victim pid=${guestPid}`))

    const health = await (await api('/health')).json()
    const hostMemoryBefore = hostAvailableMemoryMiB()
    const headroom = Math.min(health.budget.limits.memoryMiB - health.usage.memoryMiB, hostMemoryBefore)
    const memoryRequest = { ...reservation(), minimumAvailableMemoryMiB: headroom + 64 }
    const memoryStart = snapshot(victim)
    const memoryResponse = await request('/exclusive/acquire', 'POST', memoryRequest)
    await setTimeout(300)
    const memoryNestedCounter = nestedProgress()
    let memoryAfter
    let hostMemoryAfter
    if (memoryResponse.ok) {
      const value = await memoryResponse.json()
      assert.equal(value.reservation.state, 'acquired')
      memoryAfter = snapshot(victim)
      assert.equal(memoryAfter.pid, memoryStart.pid, 'Paging preserves the VMM process')
      assert.ok(memoryAfter.resident + 32 * 1024 ** 2 < memoryStart.resident, 'Actual paused VMM residency decreases')
      const qualified = await (await api('/health')).json()
      assert.ok(qualified.budget.limits.memoryMiB - qualified.usage.memoryMiB >= memoryRequest.minimumAvailableMemoryMiB, 'Confirmed cgroup headroom meets the requested target')
      hostMemoryAfter = hostAvailableMemoryMiB()
      assert.ok(hostMemoryAfter >= memoryRequest.minimumAvailableMemoryMiB, 'Confirmed host headroom meets the requested target')
      await api('/exclusive/release', 'POST', { id: memoryRequest.id, epoch: memoryRequest.epoch })
    }
    else {
      assert.equal(memoryRequired, false, `Live paging qualification was required: ${memoryResponse.status} ${await memoryResponse.text()}`)
      assert.ok([409, 503].includes(memoryResponse.status))
      assert.equal((await (await api('/exclusive')).json()).reservation, null, 'Failed memory acquisition releases every paused neighbor')
    }

    await requireNestedProgress(memoryNestedCounter)

    const expiring = { ...reservation(), expiresAt: Date.now() + 1500 }
    await api('/exclusive/acquire', 'POST', expiring)
    await setTimeout(300)
    const expiringNestedCounter = nestedProgress()
    await until(async () => (await (await api('/exclusive')).json()).reservation === null, 15000)
    await requireNestedProgress(expiringNestedCounter)
    const finalNestedCounter = nestedProgress()
    await Promise.all([owner, victim].map(entry => writeFile(path.join(entry.source, 'chat-input/messages.json'), '[{"text":"finish"}]')))
    for (const entry of [owner, victim]) {
      const result = await (await api(`/runs/${entry.id}/wait`, 'POST')).json()
      assert.equal(result.StatusCode, 0, logs(entry))
      assert.match(logs(entry), new RegExp(`exclusive-live-ok ${entry.mode} pid=\\d+`))
      await api(`/runs/${entry.id}`, 'DELETE')
    }

    process.stdout.write(`${JSON.stringify({
      root,
      image,
      before,
      memoryStart,
      memoryAfter,
      hostMemoryBeforeMiB: hostMemoryBefore,
      hostMemoryAfterMiB: hostMemoryAfter,
      nestedProgress: {
        pid: nestedPid,
        afterCpuPause: pausedNestedCounter.toString(),
        afterPagingPause: memoryNestedCounter.toString(),
        afterExpiryPause: expiringNestedCounter.toString(),
        final: finalNestedCounter.toString(),
      },
      android,
      memoryRequired,
      scope: 'Real Firecracker and ublk; same active commands, nested vCPU, guest RAM contents, checkpoint arbitration, admission and reservation expiry. Private synthetic local storage origin.',
    })}\n`)
  }
  finally {
    const controller = spawnSync('docker', ['--context', 'default', 'logs', name], { encoding: 'utf8', timeout: 10000, maxBuffer: 32 * 1024 ** 2 })
    await writeFile(path.join(root, 'controller.log'), `${controller.stdout || ''}\n${controller.stderr || ''}`)
    try {
      docker('rm', '-f', name)
    }
    catch {}

    console.error(JSON.stringify({ fixtureRoot: root }))
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
