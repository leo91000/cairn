import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { spawn, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { it, onTestFinished } from 'vitest'

const durable = fileURLToPath(new URL('../ci-backend-durable.mjs', import.meta.url))
const foreground = fileURLToPath(new URL('../ci-backend-trace.mjs', import.meta.url))
const sha = bytes => createHash('sha256').update(bytes).digest('hex')

function identity(pid) {
  try {
    const fields = readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1].split(' ')
    return { start: fields[19], state: fields[0] }
  }
  catch { return undefined }
}

async function action(t, {
  volume = 65536,
  stream = 'stdout',
  code = 7,
  mode = 'unread',
  sdk = 'success',
  timeout = false,
  checkpointDuringCleanup = false,
  traceError = false,
  missingTrace = false,
  lifecycleError = false,
  incomplete = false,
  blockedTraceClose = false,
  numericRetirementControl = false,
  receiptMode = 'complete',
  cancel,
} = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-action-exit-'))
  onTestFinished(() => rmSync(root, { recursive: true, force: true }))
  for (const name of ['proof', 'publication', 'remote', 'staging'])
    mkdirSync(path.join(root, name))
  if (blockedTraceClose) {
    const result = spawnSync('python3', ['-c', 'import os,sys\nfor i in range(4): os.mkfifo(sys.argv[1]+"/fifo"+str(i))', root])
    assert.equal(result.status, 0)
  }

  const log = path.join(root, missingTrace ? 'absent/backend-tests.log' : 'backend-tests.log')
  if (lifecycleError)
    writeFileSync(path.join(root, 'proof/action-lifecycle.json.tmp'), 'retain-existing-marker')
  const uploader = path.join(root, 'uploader.mjs')
  writeFileSync(uploader, `
    import fs from 'node:fs';
    const p=JSON.parse(fs.readFileSync(process.env.INPUT_PATH));
    const owned={pid:process.pid,start:fs.readFileSync('/proc/self/stat','utf8').split(') ')[1].split(' ')[19]};
    fs.writeFileSync(${JSON.stringify(path.join(root, 'publication'))}+'/'+process.env.INPUT_NAME+'.pid',JSON.stringify(owned));
    if(${JSON.stringify(sdk)}==='fail'){console.error(process.env.ACTIONS_RUNTIME_TOKEN);process.exit(9)}
    const receipt=${JSON.stringify(path.join(root, 'remote'))}+'/'+process.env.INPUT_NAME+'.json';
    const stagedReceipt=${JSON.stringify(path.join(root, 'staging'))}+'/'+process.env.INPUT_NAME+'.partial';
    if(${JSON.stringify(receiptMode)}==='interrupted'&&p.acquisition==='finished'){
      fs.writeFileSync(stagedReceipt,JSON.stringify(p).slice(0,32),{flag:'wx'});
      const fields=fs.readFileSync('/proc/self/stat','utf8').split(') ')[1].split(' ');
      fs.writeFileSync(${JSON.stringify(path.join(root, 'hanging'))},JSON.stringify({pid:process.pid,start:fields[19]}));
      setInterval(()=>{},1000);
    }else{
      fs.writeFileSync(stagedReceipt,JSON.stringify(p),{flag:'wx'});
      // A cancelled or size-capped write must not look like a committed upload.
      fs.renameSync(stagedReceipt,receipt);
      fs.appendFileSync(process.env.GITHUB_OUTPUT,'artifact-id=123\\nartifact-digest='+'a'.repeat(64)+'\\n');
      if(p.acquisition===${JSON.stringify(cancel ?? (sdk === 'linger' ? 'finished' : sdk === 'periodic-expiry' ? 'periodic' : 'never'))}){
        const fields=fs.readFileSync('/proc/self/stat','utf8').split(') ')[1].split(' ');
        fs.writeFileSync(${JSON.stringify(path.join(root, 'hanging'))},JSON.stringify({pid:process.pid,start:fields[19]}));
        setInterval(()=>{},1000);
      }
    }
  `)
  const waitForClose = blockedTraceClose ? `from pathlib import Path\nwhile not Path(${JSON.stringify(path.join(root, 'close-go'))}).exists(): time.sleep(.01)` : ''
  const workload = `import sys,time\np=b'x'*${volume}\nsys.${stream}.buffer.write(p)\nsys.${stream}.buffer.flush()\n${waitForClose}\n${timeout || cancel === 'periodic' || sdk === 'periodic-expiry' ? 'time.sleep(30)' : ''}\nraise SystemExit(${code})`
  const worker = path.join(root, 'worker.mjs')
  writeFileSync(worker, `
    import fs from 'node:fs';
    import {artifactPublisher,runBackendAction} from ${JSON.stringify(durable)};
    import {foregroundAction} from ${JSON.stringify(foreground)};
    void foregroundAction(async()=>{
      process.on('SIGXFSZ',()=>{});
      // Deliberately fill the advisory public pipes as well as exercising the
      // real private trace. Production forwards no raw workload bytes here.
      process.stdout.write(Buffer.alloc(65536,'p'));process.stderr.write(Buffer.alloc(65536,'q'));
      if(${blockedTraceClose}){
        const watch=setInterval(()=>{
          if(!fs.existsSync(${JSON.stringify(log)})||fs.statSync(${JSON.stringify(log)}).size<${volume})return;
          clearInterval(watch);
          for(let i=0;i<4;i++)fs.open(${JSON.stringify(root)}+'/fifo'+i,'r',()=>{});
          setTimeout(()=>fs.writeFileSync(${JSON.stringify(path.join(root, 'close-go'))},'go'),50);
        },10);
      }
      const startedAt=Date.now()-28*60000+${timeout ? 600 : 60000};
      const env={PATH:process.env.PATH,HOME:${JSON.stringify(root)},GITHUB_RUN_ATTEMPT:'1',CI_JOB_STARTED_AT:startedAt/1000,ACTIONS_RUNTIME_TOKEN:'synthetic-private-token'};
      const publish=artifactPublisher({uploader:${JSON.stringify(uploader)},directory:${JSON.stringify(path.join(root, 'publication'))},env,uploadMs:300,closeMs:50});
      const checkpointDelivery={armed:false,delivered:false,cancelled:false};
      if(${checkpointDuringCleanup}){
        // Deliver the pending checkpoint only after the real guard persists
        // its deadline, while owned-process cleanup is still in progress.
        // This controls the phase, not the guard or publication clock/bounds.
        const schedule=globalThis.setTimeout, cancel=globalThis.clearTimeout;
        let pending;
        globalThis.setTimeout=(callback,ms,...args)=>{
          if(ms!==30||checkpointDelivery.armed)return schedule(callback,ms,...args);
          checkpointDelivery.armed=true;
          pending=setInterval(()=>{
            const proof=JSON.parse(fs.readFileSync(${JSON.stringify(path.join(root, 'proof/latest.json'))}));
            if(!proof.timedOut)return;
            clearInterval(pending);
            checkpointDelivery.delivered=true;
            callback(...args);
          },1);
          return pending;
        };
        globalThis.clearTimeout=timer=>{
          if(timer===pending){checkpointDelivery.cancelled=true;clearInterval(timer)}
          else cancel(timer);
        };
      }
      const result=await runBackendAction({log:${JSON.stringify(log)},output:${JSON.stringify(path.join(root, 'proof'))},command:'python3',args:['-c',${JSON.stringify(workload)}],env,
        receipt:{clock:{jobId:123,startedAt},qualificationClaim:false},publish,firstCheckpointMs:30,checkpointMs:40,
        observeOptions:{sampleMs:20,graceMs:30,closeGraceMs:100,inventoryLimit:${incomplete ? 1 : 4096}}});
      fs.writeFileSync(${JSON.stringify(path.join(root, 'returned.json'))},JSON.stringify(result));
      fs.writeFileSync(${JSON.stringify(path.join(root, 'checkpoint-delivery.json'))},JSON.stringify(checkpointDelivery));
      return ${numericRetirementControl ? '125' : 'result'};
    });
  `)
  const args = traceError
    ? ['-c', 'import os,resource,sys;resource.setrlimit(resource.RLIMIT_FSIZE,(16384,16384));os.execvp("node",["node",sys.argv[1]])', worker]
    : [worker]
  const started = Date.now()
  const child = spawn(traceError ? 'python3' : process.execPath, args, {
    stdio: ['ignore', 'pipe', 'pipe'],
    env: { ...process.env, UV_THREADPOOL_SIZE: '4' },
  })
  const pinned = identity(child.pid)
  let exit
  const exited = new Promise(resolve => child.once('exit', (code, signal) => {
    exit = { code, signal }
    resolve(exit)
  }))
  const closed = new Promise(resolve => child.once('close', resolve))
  let slow
  if (mode === 'closed') {
    child.stdout.destroy()
    child.stderr.destroy()
  }
  else if (mode === 'slow') {
    slow = setInterval(() => {
      child.stdout.read(1024)
      child.stderr.read(1024)
    }, 30)
  }
  else if (mode === 'stopped') {
    for (const output of [child.stdout, child.stderr]) {
      output.once('data', () => output.pause())
      output.resume()
    }
  }

  try {
    if (cancel) {
      const until = Date.now() + 2000
      while (!existsSync(path.join(root, 'hanging')) && Date.now() < until && child.exitCode === null && child.signalCode === null)
        await delay(10)
      assert.ok(existsSync(path.join(root, 'hanging')), 'did not reach the real credentialed publisher cancellation seam')
      if (identity(child.pid)?.start === pinned?.start)
        child.kill('SIGTERM')
    }

    const result = await Promise.race([exited, delay(numericRetirementControl ? 3500 : 2500).then(() => null)])
    if (numericRetirementControl) {
      assert.equal(result, null, 'numeric-exit control unexpectedly completed without retiring blocked local I/O')
      const returned = JSON.parse(readFileSync(path.join(root, 'returned.json'), 'utf8'))
      assert.equal(returned.guardReturn.code, 0)
      assert.equal(returned.trace.complete, false)
      assert.equal(returned.ownedClosureVerified, true)
      assert.equal(returned.publisherClosureVerified, true)
      return { aliveAtBound: true, returned }
    }

    assert.ok(result, 'foreground action did not autonomously exit with public readers stopped')
    assert.equal(result.signal, blockedTraceClose ? 'SIGKILL' : null)
    const returned = JSON.parse(readFileSync(path.join(root, 'returned.json'), 'utf8'))
    if (receiptMode === 'interrupted')
      assert.equal(returned.publisherClosureVerified, false)
    const receipts = readdirSync(path.join(root, 'remote')).map(name => JSON.parse(readFileSync(path.join(root, 'remote', name), 'utf8')))
    const trace = existsSync(log) ? readFileSync(log) : undefined
    for (const name of readdirSync(path.join(root, 'publication')).filter(name => name.endsWith('.pid'))) {
      const helper = JSON.parse(readFileSync(path.join(root, 'publication', name), 'utf8'))
      assert.ok(!identity(helper.pid) || identity(helper.pid).start !== helper.start || identity(helper.pid).state === 'Z', 'credentialed publisher survived foreground exit')
    }

    if (existsSync(path.join(root, 'hanging'))) {
      const helper = JSON.parse(readFileSync(path.join(root, 'hanging'), 'utf8'))
      assert.ok(!identity(helper.pid) || identity(helper.pid).start !== helper.start || identity(helper.pid).state === 'Z', 'credentialed publisher survived foreground exit')
    }

    return {
      ...result,
      root,
      returned,
      checkpointDelivery: JSON.parse(readFileSync(path.join(root, 'checkpoint-delivery.json'), 'utf8')),
      receipts,
      stagedReceipts: readdirSync(path.join(root, 'staging')),
      trace,
      elapsed: Date.now() - started,
    }
  }
  finally {
    clearInterval(slow)
    if (!exit && identity(child.pid)?.start === pinned?.start)
      child.kill('SIGKILL')
    child.stdout.destroy()
    child.stderr.destroy()
    await closed
  }
}

for (const mode of ['unread', 'stopped', 'slow', 'closed']) {
  for (const stream of ['stdout', 'stderr']) {
    for (const volume of [1, 1048576]) {
      for (const code of [0, 7]) {
        it(`autonomously exits${code} with exact${volume}-byte private${stream}, public pipes${mode}`, async (t) => {
          const result = await action(t, {
            mode,
            stream,
            volume,
            code,
          })
          assert.equal(result.code, code)
          const expected = Buffer.alloc(volume, 'x')
          assert.deepEqual(result.trace, expected)
          assert.equal(result.returned.guardReturn.code, code)
          assert.equal(result.returned.trace.writtenBytes, volume)
          assert.equal(result.returned.trace.sha256, sha(expected))
          assert.equal(result.returned.trace.complete, true)
          assert.equal(result.returned.foregroundExit, 'not-observed')
          const final = result.receipts.find(item => item.acquisition === 'finished')
          assert.equal(final.guardReturn.code, code)
          assert.equal(final.trace.complete, true)
          assert.equal(final.trace.sha256, sha(expected))
          assert.equal(final.foregroundExit, 'not-observed')
        })
      }
    }
  }
}

it('preserves the full ten-megabyte failure trace with unread public pipes', async (t) => {
  const result = await action(t, { volume: 10000000 })
  assert.equal(result.code, 7)
  assert.equal(result.trace.length, 10000000)
  assert.equal(sha(result.trace), sha(Buffer.alloc(10000000, 'x')))
})

it('keeps an actual guard timeout non-green after private trace closure', async (t) => {
  const result = await action(t, { timeout: true, checkpointDuringCleanup: true })
  assert.equal(result.code, 124)
  assert.equal(result.returned.guardReturn.code, 124)
  assert.equal(result.returned.trace.complete, true)
  assert.deepEqual(result.trace, Buffer.alloc(65536, 'x'))
  assert.deepEqual(result.checkpointDelivery, { armed: true, delivered: false, cancelled: true })
  assert.equal(result.returned.ownedClosureVerified, true)
  assert.equal(result.returned.publisherClosureVerified, true)
  assert.equal(result.returned.finalPublicationReturned, true)
  const final = result.receipts.find(item => item.acquisition === 'finished')
  assert.equal(final.guardReturn.code, 124)
  assert.equal(final.proof.timedOut, true)
  assert.equal(final.proof.samples.at(-1).processCount, 0)
  assert.equal(final.trace.complete, true)
})

it('keeps a real periodic uploader expiry non-green even when the guard closes its workload', async (t) => {
  const result = await action(t, { sdk: 'periodic-expiry' })
  assert.equal(result.code, 125)
  assert.equal(result.returned.guardReturn.code, 130)
  assert.equal(result.returned.trace.complete, true)
  assert.equal(result.returned.ownedClosureVerified, true)
  assert.equal(result.returned.publisherClosureVerified, false)
  assert.equal(result.returned.finalPublicationReturned, false)
})

it('bounds a real private-file close blocked behind process-local filesystem workers and exits non-green', async (t) => {
  const result = await action(t, { code: 0, volume: 1, blockedTraceClose: true })
  assert.equal(result.code, null)
  assert.equal(result.signal, 'SIGKILL')
  assert.equal(result.returned.guardReturn.code, 0)
  assert.equal(result.returned.trace.complete, false)
  assert.equal(result.returned.trace.state, 'unknown')
  assert.equal(result.returned.exitDecision.kind, 'signal')
  assert.equal(result.returned.ownedClosureVerified, true)
  assert.equal(result.returned.publisherClosureVerified, true)
  assert.deepEqual(result.trace, Buffer.from('x'))
})

it('retains a red-capable control: numeric process.exit alone can wait on the blocked local FS pool', async (t) => {
  const result = await action(t, {
    code: 0,
    volume: 1,
    blockedTraceClose: true,
    numericRetirementControl: true,
  })
  assert.equal(result.aliveAtBound, true)
  assert.equal(result.returned.exitDecision.kind, 'signal')
})

it('does not treat an interrupted local upload as a committed receipt', async (t) => {
  const result = await action(t, {
    code: 0,
    cancel: 'finished',
    receiptMode: 'interrupted',
  })
  assert.equal(result.code, 130)
  assert.equal(result.returned.publisherClosureVerified, false)
  assert.equal(result.receipts.some(receipt => receipt.acquisition === 'finished'), false)
  assert.equal(result.stagedReceipts.length, 1)
})

for (const [name, options] of [
  ['trace-open failure', { missingTrace: true }],
  ['actual trace-write failure under a process-only file-size cap', { traceError: true }],
  ['failed evidence publication', { sdk: 'fail' }],
  ['committed publisher which does not close', { sdk: 'linger' }],
  ['incomplete owned-process inventory', { incomplete: true }],
  ['failed lifecycle record', { lifecycleError: true }],
]) {
  it(`never exits green on${name}`, async (t) => {
    const result = await action(t, { ...options, code: 0 })
    assert.equal(result.code, 125)
    if (options.missingTrace || options.traceError)
      assert.equal(result.returned.trace.complete, false)
  })
}

for (const cancel of ['admitted-before-command', 'periodic', 'finished']) {
  it(`autonomously cancels${cancel} without a credentialed publisher leak`, async (t) => {
    const result = await action(t, { cancel, code: 0 })
    assert.equal(result.code, 130)
    assert.equal(result.returned.trace.complete, true)
  })
}
