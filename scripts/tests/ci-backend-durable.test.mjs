import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout as delay } from 'node:timers/promises'
import { fileURLToPath } from 'node:url'
import { runInNewContext } from 'node:vm'
import { it, onTestFinished, vi } from 'vitest'
import { parse } from 'yaml'
import {
  artifactPublisher,
  BACKEND_ARGS,
  BACKEND_COMMAND,
  checkpoint,
  PUBLICATION_LIMIT,
  readProof,
  runDurableBackend,
  UPLOADER_SHA,
} from '../ci-backend-durable.mjs'

function fixture(t, mode = 'success', periodicDelayMs = 0) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'cairn-durable-'))
  onTestFinished(() => rmSync(root, { recursive: true, force: true }))
  const output = path.join(root, 'proof')
  const directory = path.join(root, 'publication')
  const sink = path.join(root, 'remote')
  for (const name of [output, directory, sink])
    mkdirSync(name)
  const uploader = path.join(root, 'uploader.mjs')
  writeFileSync(uploader, `
    import fs from 'node:fs';
    const mode=${JSON.stringify(mode)};
    const hang=()=>{fs.writeFileSync(${JSON.stringify(path.join(root, 'hanging-uploader'))},JSON.stringify({pid:process.pid,start:fs.readFileSync('/proc/self/stat','utf8').split(') ')[1].split(' ')[19]}));process.on('SIGTERM',()=>{});setInterval(()=>{},1000)};
    if(mode==='hang'){hang()}
    else if(mode==='failure'){console.error(process.env.ACTIONS_RUNTIME_TOKEN);process.exit(7)}
    else if(mode==='no-receipt'){process.exit(0)}
    else{
      const file=process.env.INPUT_PATH;
      const data=fs.readFileSync(file);
      if(${periodicDelayMs}>0&&JSON.parse(data).acquisition==='periodic') await new Promise(resolve=>setTimeout(resolve,${periodicDelayMs}));
      if(mode==='periodic-failure'&&JSON.parse(data).acquisition==='periodic'){console.error(process.env.ACTIONS_RUNTIME_TOKEN);process.exit(7)}
      if(mode===JSON.parse(data).acquisition+'-hang'){hang();await new Promise(()=>{})}
      fs.writeFileSync(${JSON.stringify(sink)}+'/'+process.env.INPUT_NAME+'.json', data, {flag:'wx'});
      fs.appendFileSync(process.env.GITHUB_OUTPUT,'artifact-id<<receipt\\n123\\nreceipt\\nartifact-digest<<digest\\n'+'a'.repeat(64)+'\\ndigest\\n');
    }
  `)
  const env = {
    PATH: process.env.PATH,
    HOME: root,
    TMPDIR: root,
    ACTIONS_RUNTIME_TOKEN: 'private-runtime-token-fixture',
    ACTIONS_ID_TOKEN_REQUEST_TOKEN: 'private-oidc-fixture',
    ACTIONS_RESULTS_URL: 'https://private-runtime-endpoint.invalid',
    GITHUB_TOKEN: 'private-github-token-fixture',
    GITHUB_RUN_ATTEMPT: '1',
    INPUT_PRIVATE: 'private-input-fixture',
  }
  const uploadMs = 1000
  const closeMs = 50
  const firstCheckpointMs = 30
  const checkpointMs = 35

  // Startup and five rolling uploads are sequential and each may use its full
  // bound. Leave the guard's one-second containment startup allowance as well;
  // only the final upload belongs in the evidence reserve after the deadline.
  const publicationsBeforeDeadline = PUBLICATION_LIMIT - 1
  const publicationWindowMs = uploadMs + closeMs + Math.max(firstCheckpointMs, checkpointMs)
  const workloadWindowMs = 1000 + publicationsBeforeDeadline * publicationWindowMs

  const receipt = {
    diagnosticOnly: true,
    qualificationClaim: false,
    observer: 'f'.repeat(40),
    candidate: '3'.repeat(40),
    clock: { jobId: 123, startedAt: Date.now() - 28 * 60000 + workloadWindowMs },
  }
  env.CI_JOB_STARTED_AT = receipt.clock.startedAt / 1000
  const publish = artifactPublisher({
    uploader,
    directory,
    env,
    uploadMs,
    closeMs,
  })
  const options = {
    output,
    env,
    receipt,
    command: process.execPath,
    args: ['-e', 'setInterval(()=>{},1000)'],
    publish,
    firstCheckpointMs,
    checkpointMs,
    observeOptions: { sampleMs: 20, graceMs: 30, closeGraceMs: 100 },
  }
  return {
    root,
    output,
    directory,
    sink,
    uploader,
    env,
    receipt,
    publish,
    options,
  }
}

function committed(sink) {
  return readdirSync(sink).sort().map(name => JSON.parse(readFileSync(path.join(sink, name), 'utf8')))
}

function current(pid, read = readFileSync) {
  let text
  try {
    text = read(`/proc/${pid}/stat`, 'utf8')
  }
  catch (error) {
    if (error.code === 'ENOENT' || error.code === 'ESRCH')
      return undefined
    throw error
  }

  const header = `${pid} (`
  const end = text.lastIndexOf(') ')
  assert.ok(text.startsWith(header) && end >= header.length, 'Invalid process identity header')
  const fields = text.slice(end + 2).trim().split(/\s+/)
  assert.ok(fields.length >= 20, 'Incomplete process identity')
  assert.match(fields[0], /^[RSDZTtXxKWPI]$/, 'Invalid process state')
  assert.match(fields[19], /^\d+$/, 'Invalid process start time')
  return { pid, start: fields[19], state: fields[0] }
}

// These helpers also drive the real fixture assertions and cleanup below.
function isLive(item, read = current) {
  const snapshot = read(item.pid)
  return snapshot?.pid === item.pid && snapshot.start === item.startTicks && snapshot.state !== 'Z'
}

function signalOwned(item, read = current, kill = process.kill) {
  if (!isLive(item, read))
    return

  try {
    kill(item.pid, 'SIGKILL')
  }
  catch (error) {
    if (error.code !== 'ESRCH')
      throw error
  }
}

async function withCleanup(body, cleanup) {
  let result
  let bodyFailure
  let bodyFailed = false
  try {
    result = await body()
  }
  catch (error) {
    bodyFailure = error
    bodyFailed = true
  }

  try {
    await cleanup()
  }
  catch (error) {
    if (bodyFailed)
      throw new AggregateError([bodyFailure, error], 'Fixture body and cleanup failed')
    throw error
  }

  if (bodyFailed)
    throw bodyFailure
  return result
}

const ownedProcess = { pid: 12345, startTicks: '67890' }
const activeProcess = { pid: ownedProcess.pid, start: ownedProcess.startTicks, state: 'S' }
const procStat = (pid = ownedProcess.pid, start = ownedProcess.startTicks, state = 'S') => `${pid} (fixture (with) spaces) ${[state, ...Array.from({ length: 18 }).fill('0'), start].join(' ')}`

it.each([1, 2])('uses one identity snapshot when a process disappears after read %s', (readsBeforeExit) => {
  let reads = 0
  const read = () => reads++ < readsBeforeExit ? activeProcess : undefined
  assert.equal(isLive(ownedProcess, read), true)
  assert.equal(reads, 1)
})

it.each([
  ['absent', undefined, false],
  ['reused', { ...activeProcess, start: '67891' }, false],
  ['another PID', { ...activeProcess, pid: 12346 }, false],
  ['zombie', { ...activeProcess, state: 'Z' }, false],
  ['same live identity', activeProcess, true],
])('keeps strict identity and liveness for %s', (_label, snapshot, expected) => {
  let reads = 0
  assert.equal(isLive(ownedProcess, () => {
    reads++
    return snapshot
  }), expected)
  assert.equal(reads, 1)
})

it('reads a complete process identity even when its command contains parentheses and spaces', () => {
  assert.deepEqual(current(ownedProcess.pid, () => procStat()), activeProcess)
})

it.each(['ENOENT', 'ESRCH'])('accepts %s as a missing process identity', (code) => {
  assert.equal(current(ownedProcess.pid, () => {
    throw Object.assign(new Error('gone'), { code })
  }), undefined)
  for (const code of ['EACCES', 'EPERM', 'EIO']) {
    const error = Object.assign(new Error('unreadable'), { code })
    assert.throws(() => current(ownedProcess.pid, () => {
      throw error
    }), caught => caught === error)
  }
})

it.each(['malformed', procStat(12346), procStat(12345, 'invalid'), procStat(12345, '67890', '?')])('rejects invalid or mismatched process identity %s', (text) => {
  assert.throws(() => current(ownedProcess.pid, () => text))
})

it.each([undefined, { ...activeProcess, start: '67891' }, { ...activeProcess, pid: 12346 }, { ...activeProcess, state: 'Z' }])('never signals a missing, reused or retired identity', (snapshot) => {
  const signals = []
  signalOwned(ownedProcess, () => snapshot, (...args) => signals.push(args))
  assert.deepEqual(signals, [])
})

it('ignores ESRCH when the matched process exits before its signal', () => {
  let reads = 0
  const signals = []
  signalOwned(ownedProcess, () => {
    reads++
    return activeProcess
  }, (...args) => {
    signals.push(args)
    throw Object.assign(new Error('gone'), { code: 'ESRCH' })
  })
  assert.equal(reads, 1)
  assert.deepEqual(signals, [[ownedProcess.pid, 'SIGKILL']])
})

it.each(['EACCES', 'EPERM', 'EIO'])('retains unexpected signal/read error %s', (code) => {
  const error = Object.assign(new Error('denied'), { code })
  assert.throws(() => signalOwned(ownedProcess, () => activeProcess, () => {
    throw error
  }), caught => caught === error)
  assert.throws(() => signalOwned(ownedProcess, () => {
    throw error
  }, () => assert.fail('signal after unreadable identity')), caught => caught === error)
})

it('preserves both a fixture body failure and an independent cleanup failure', async () => {
  const body = new Error('receipt assertion failed')
  const cleanup = new Error('owned process still live')
  await assert.rejects(withCleanup(async () => {
    throw body
  }, async () => {
    throw cleanup
  }), error => error instanceof AggregateError && error.errors[0] === body && error.errors[1] === cleanup)
})

it('preserves individual fixture outcomes while always running cleanup', async () => {
  const body = new Error('body failed')
  const cleanup = new Error('cleanup failed')
  let cleaned = false
  await assert.rejects(withCleanup(async () => {
    throw body
  }, async () => {
    cleaned = true
  }), error => error === body)
  assert.equal(cleaned, true)
  await assert.rejects(withCleanup(async () => 7, async () => {
    throw cleanup
  }), error => error === cleanup)
  assert.equal(await withCleanup(() => 7, () => {}), 7)
})

// A 250 ms upload fits its individual bound, but five cannot fit the old
// one-second window even without contention. Exercise both publisher timings.
it.each([0, 250])('commits source/clock/startup before the real child, checkpoints a never-exiting child, and keeps timeout nonzero (periodic upload delay %i ms)', async (periodicDelayMs, t) => {
  const f = fixture(t, 'success', periodicDelayMs)
  const publish = f.options.publish
  let sampledWorkload = false
  f.options.publish = async (payload, options) => {
    const result = await publish(payload, options)
    if (payload.acquisition === 'periodic' && !sampledWorkload) {
      // Rolling uploads begin before containment startup finishes. Hold the
      // first return until the real workload is sampled, so later checkpoints
      // exercise live processes even when all uploads finish quickly.
      await vi.waitFor(() => {
        const proof = readProof(path.join(f.output, 'latest.json'))
        assert.ok(proof.samples.some(sample => sample.processCount >= 2))
      }, { timeout: 2000, interval: 10 })
      sampledWorkload = true
    }

    return result
  }

  const marker = path.join(f.root, 'child.json')
  f.options.args = ['-e', `
    const fs=require('node:fs');
    if(!fs.existsSync(${JSON.stringify(path.join(f.sink, 'backend-diagnostic-1-1.json'))})) process.exit(91);
    const leaked=Object.keys(process.env).filter(k=>k.startsWith('ACTIONS_')||k.startsWith('INPUT_')||['GITHUB_TOKEN','GITHUB_OUTPUT','GITHUB_ENV'].includes(k));
    fs.writeFileSync(${JSON.stringify(marker)}, JSON.stringify({leaked}));
    setInterval(()=>{},1000);
  `]
  assert.equal(await runDurableBackend(f.options), 124)
  assert.deepEqual(JSON.parse(readFileSync(marker, 'utf8')), { leaked: [] })
  const receipts = committed(f.sink)
  assert.equal(receipts.length, PUBLICATION_LIMIT)
  assert.equal(receipts[0].acquisition, 'admitted-before-command')
  assert.deepEqual(receipts[0].clock, f.receipt.clock)
  assert.equal(receipts[0].proof.samples.at(-1).processCount, 0)

  const finalProof = receipts.at(-1).proof
  assert.equal(finalProof.timedOut, true)
  assert.equal(finalProof.cleanupUnknown, false)
  assert.equal(finalProof.inventoryComplete, true)
  assert.equal(finalProof.ownershipUncertain, false)
  assert.equal(finalProof.containmentReady, true)
  assert.equal(finalProof.samples.at(-1).reason, 'finished')
  assert.equal(finalProof.samples.at(-1).processCount, 0)

  assert.ok(receipts.some(item => item.acquisition === 'periodic' && item.proof.samples.some(sample => sample.processCount >= 2)))
  assert.ok(receipts.every(item => item.qualificationClaim === false))
  for (const item of receipts.flatMap(item => item.proof.samples).flatMap(sample => sample.processes))
    assert.ok(!isLive(item))
  for (const file of readdirSync(f.directory)) {
    const text = readFileSync(path.join(f.directory, file), 'utf8')
    assert.ok(!text.includes('private-runtime-token-fixture'))
    assert.ok(!text.includes('private-oidc-fixture'))
  }
})

it('preserves a committed checkpoint when cancellation skips the normal workflow after-step', async (t) => {
  const f = fixture(t)
  const signal = new AbortController()
  f.options.signal = signal.signal
  f.options.receipt.clock.startedAt = Date.now() - 27 * 60000
  f.env.CI_JOB_STARTED_AT = f.receipt.clock.startedAt / 1000
  f.options.publish = async (payload) => {
    const result = await f.publish(payload)
    if (payload.acquisition === 'periodic')
      signal.abort()
    return result
  }

  assert.equal(await runDurableBackend(f.options), 130)
  assert.ok(committed(f.sink).some(item => item.acquisition === 'periodic'))
  assert.ok(!existsSync(path.join(f.root, 'normal-after-step')))
})

it('preserves the workload failure even when all evidence commits succeed', async (t) => {
  const f = fixture(t)
  assert.equal(await runDurableBackend({ ...f.options, args: ['-e', 'process.exit(7)'] }), 7)
  assert.equal(committed(f.sink).at(-1).acquisition, 'finished')
  assert.equal(committed(f.sink).at(-1).qualificationClaim, false)
})

it('rechecks the API clock after a startup remote commit before launching the workload', async (t) => {
  const f = fixture(t)
  f.receipt.clock.startedAt = Date.now() - 28 * 60000 + 200
  f.env.CI_JOB_STARTED_AT = f.receipt.clock.startedAt / 1000
  const marker = path.join(f.root, 'too-late-child')
  const status = await runDurableBackend({
    ...f.options,
    args: ['-e', `require('node:fs').writeFileSync(${JSON.stringify(marker)},'bad')`],
    publish: async (payload, options) => {
      const result = await f.publish(payload, options)
      if (payload.acquisition === 'admitted-before-command')
        await delay(250)
      return result
    },

  })
  assert.equal(status, 124)
  assert.ok(!existsSync(marker))
  assert.equal(committed(f.sink)[0].acquisition, 'admitted-before-command')
  assert.equal(committed(f.sink).at(-1).proof.timedOut, true)
})

it('bounds each uploader by the actual remaining API budget', async (t) => {
  const f = fixture(t, 'hang')
  f.receipt.clock.startedAt = Date.now() - 28 * 60000 + 120
  f.env.CI_JOB_STARTED_AT = f.receipt.clock.startedAt / 1000
  const publish = artifactPublisher({
    uploader: f.uploader,
    directory: f.directory,
    env: f.env,
    uploadMs: 5000,
    closeMs: 20,
  })
  const started = Date.now()
  await assert.rejects(publish({ clock: f.receipt.clock, acquisition: 'admitted-before-command' }), /failed|unknown/)
  assert.ok(Date.now() - started < 500)
  const pid = JSON.parse(readFileSync(path.join(f.root, 'hanging-uploader'), 'utf8'))
  assert.ok(!isLive({ pid: pid.pid, startTicks: pid.start }))
})

for (const mode of ['hang', 'periodic-hang', 'finished-hang']) {
  it(`cancels the credentialed uploader during ${mode} without leaking a helper or returning success`, async (t) => {
    const f = fixture(t, mode)
    f.receipt.clock.startedAt = Date.now() - 27 * 60000
    f.env.CI_JOB_STARTED_AT = f.receipt.clock.startedAt / 1000
    const signal = new AbortController()
    const result = runDurableBackend({
      ...f.options,
      signal: signal.signal,
      args: mode === 'finished-hang' ? ['-e', 'process.exit(0)'] : f.options.args,
    })
    await withCleanup(async () => {
      const marker = path.join(f.root, 'hanging-uploader')
      // The final publisher follows the startup upload, containment startup and
      // command completion; one upload's bound cannot cover that whole path.
      await vi.waitFor(() => assert.ok(existsSync(marker)), { timeout: 3000, interval: 10 })
      signal.abort()
      assert.equal(await result, 130)
      const pid = JSON.parse(readFileSync(marker, 'utf8'))
      assert.ok(!isLive({ pid: pid.pid, startTicks: pid.start }))
      const proof = readProof(path.join(f.output, 'latest.json'))
      assert.equal(proof.samples.at(-1).processCount, 0)
    }, async () => {
      signal.abort()
      await result
    })
  })
}

it('retains remote receipts when the foreground parent is killed, with fixture-owned descendants cleaned separately', async (t) => {
  const f = fixture(t)
  const module = fileURLToPath(new URL('../ci-backend-durable.mjs', import.meta.url))
  const worker = path.join(f.root, 'worker.mjs')
  writeFileSync(worker, `
    import {artifactPublisher,runDurableBackend} from ${JSON.stringify(module)};
    const options=${JSON.stringify({
      output: f.output,
      command: process.execPath,
      args: ['-e', 'setInterval(()=>{},1000)'],
      env: {},
      receipt: { ...f.receipt, clock: { jobId: 123, startedAt: Date.now() - 27 * 60000 } },
      firstCheckpointMs: 30,
      checkpointMs: 1000,
      observeOptions: { sampleMs: 20, graceMs: 30 },
    })};
    const publisher=${JSON.stringify({
      uploader: f.uploader,
      directory: f.directory,
      env: f.env,
      uploadMs: 1000,
    })};
    publisher.env.CI_JOB_STARTED_AT=options.receipt.clock.startedAt/1000;
    options.publish=artifactPublisher(publisher);
    process.exitCode=await runDurableBackend(options);
  `)
  const parent = spawn(process.execPath, [worker], { stdio: 'ignore' })
  const closed = new Promise(resolve => parent.once('close', (code, signal) => resolve({ code, signal })))
  let owned = []
  await withCleanup(async () => {
    const until = Date.now() + 2000
    while (Date.now() < until) {
      const receipts = committed(f.sink)
      owned = receipts.flatMap(item => item.proof.samples).flatMap(sample => sample.processes)
      if (receipts.some(item => item.acquisition === 'periodic') && owned.length >= 2)
        break
      await delay(10)
    }

    assert.ok(owned.length >= 2)
    parent.kill('SIGKILL')
    assert.deepEqual(await closed, { code: null, signal: 'SIGKILL' })
    const receipts = committed(f.sink)
    assert.equal(receipts[0].candidate, f.receipt.candidate)
    assert.ok(receipts.some(item => item.acquisition === 'periodic'))
    assert.ok(!receipts.some(item => item.acquisition === 'finished'))
  }, async () => {
    parent.kill('SIGKILL')
    for (const item of owned)
      signalOwned(item)

    await closed
    const until = Date.now() + 1000
    while (Date.now() < until && owned.some(item => isLive(item)))
      await delay(10)
    assert.ok(owned.every(item => !isLive(item)))
  })
})

it.each(['failure', 'no-receipt', 'hang'])('fails closed before any heavy child on uploader %s', async (mode, t) => {
  const f = fixture(t, mode)
  const marker = path.join(f.root, 'child-started')
  const publish = artifactPublisher({
    uploader: f.uploader,
    directory: f.directory,
    env: f.env,
    uploadMs: 80,
    closeMs: 50,
  })
  const started = Date.now()
  assert.equal(await runDurableBackend({ ...f.options, publish, args: ['-e', `require('node:fs').writeFileSync(${JSON.stringify(marker)},'bad')`] }), 125)
  assert.ok(!existsSync(marker))
  assert.ok(Date.now() - started < 1000)
})

it('stops and cleans the active workload when a rolling upload fails instead of continuing without evidence', async (t) => {
  const f = fixture(t, 'periodic-failure')
  assert.equal(await runDurableBackend(f.options), 125)
  assert.equal(committed(f.sink).length, 1)
  const proof = readProof(path.join(f.output, 'latest.json'))
  assert.equal(proof.interrupted, true)
  assert.equal(proof.cleanupUnknown, false)
  assert.equal(proof.samples.at(-1).processCount, 0)
})

it('discards a real uploader error containing a runtime token without copying it to parent output or retained files', async (t) => {
  const f = fixture(t, 'failure')
  const captured = []
  const stdout = vi.spyOn(process.stdout, 'write').mockImplementation((bytes) => {
    captured.push(bytes.toString())
    return true
  })
  const stderr = vi.spyOn(process.stderr, 'write').mockImplementation((bytes) => {
    captured.push(bytes.toString())
    return true
  })
  let status
  try {
    status = await runDurableBackend(f.options)
  }
  finally {
    stdout.mockRestore()
    stderr.mockRestore()
  }

  assert.equal(status, 125)
  assert.ok(!captured.join('').includes(f.env.ACTIONS_RUNTIME_TOKEN))
  for (const name of readdirSync(f.directory))
    assert.ok(!readFileSync(path.join(f.directory, name), 'utf8').includes(f.env.ACTIONS_RUNTIME_TOKEN))
})

it('rejects an eighth publication instead of overwriting evidence or exhausting the job artifact allowance', async (t) => {
  const f = fixture(t)
  for (let index = 0; index < PUBLICATION_LIMIT; index++)
    await f.publish({ sequence: index, clock: f.receipt.clock })
  await assert.rejects(f.publish({ sequence: 8, clock: f.receipt.clock }), /publication stopped/)
  assert.equal(readdirSync(f.sink).length, 7)
})

it('projects fixed numeric/enumerated evidence and refuses linked/oversized proof files', (t) => {
  const f = fixture(t)
  const secret = 'sensitive-payload-never-retained'
  const proof = checkpoint({
    environment: secret,
    jobStartedAt: 1,
    deadline: 2,
    timedOut: false,
    interrupted: false,
    inventoryComplete: true,
    ownershipUncertain: false,
    cleanupUnknown: false,
    containmentReady: true,
    samples: [{
      phase: secret,
      reason: secret,
      cmdline: secret,
      processes: [{
        role: secret,
        startTicks: secret,
        cpuTicks: 2,
        io: { read_bytes: 12, secret },
        cgroup: { cpu: { usage_usec: 3 } },
      }],
    }],
  })
  assert.ok(!JSON.stringify(proof).includes(secret))
  assert.equal(proof.samples[0].processes[0].cpuTicks, 2)
  const real = path.join(f.root, 'real')
  writeFileSync(real, '{}')
  symlinkSync(real, path.join(f.output, 'latest.json'))
  assert.throws(() => readProof(path.join(f.output, 'latest.json')))
  symlinkSync(f.output, path.join(f.root, 'linked'))
  assert.throws(() => readProof(path.join(f.root, 'linked/latest.json')))
  writeFileSync(real, ' '.repeat(262145))
  assert.throws(() => readProof(real), /bound|Invalid/)
})

it('keeps exact normal/diagnostic commands, permissions and the cumulative artifact bound across outcomes', () => {
  const yaml = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  const backend = yaml.jobs.backend
  const action = parse(readFileSync(new URL('../../.github/actions/backend-durable-evidence/action.yaml', import.meta.url), 'utf8'))
  const observer = parse(readFileSync(new URL('../../.github/actions/observe-rust-build/action.yaml', import.meta.url), 'utf8'))
  assert.equal(backend['timeout-minutes'], 30)
  assert.deepEqual(backend.permissions, { contents: 'read', actions: 'read' })
  assert.equal(action.runs.using, 'node24')
  const checkout = backend.steps.find(step => step.with?.repository === 'actions/upload-artifact')
  assert.equal(checkout.with.ref, UPLOADER_SHA)
  assert.equal(checkout.with['persist-credentials'], false)
  const normal = backend.steps.find(step => step.name === 'Run backend tests across binaries with isolated credentials')
  assert.ok(normal.run.includes(`${BACKEND_COMMAND} ${BACKEND_ARGS.join(' ')} 2>&1 | tee backend-tests.log`))
  const artifactCount = (steps, diagnostic) => steps.reduce((count, step) => {
    if (step.if && !['always()', 'inputs.ci-diagnostic == \'backend\'', 'inputs.ci-diagnostic != \'backend\''].includes(step.if))
      return count
    const selected = !step.if || step.if === 'always()' || runInNewContext(step.if.replaceAll('inputs.ci-diagnostic', 'diagnostic'), { diagnostic })
    if (!selected)
      return count
    if (step.uses?.startsWith('actions/upload-artifact@'))
      return count + 1
    if (step.uses === './.github/actions/observe-rust-build')
      return count + artifactCount(observer.runs.steps, diagnostic)
    return count + (step.uses === './.github/actions/backend-durable-evidence' ? PUBLICATION_LIMIT : 0)
  }, 0)
  for (const mode of ['backend', 'none', '']) {
    for (const outcome of ['success', 'failure', 'cancelled']) {
      const count = artifactCount(backend.steps, mode)
      assert.ok(count <= 10, `${mode}/${outcome}: ${count}`)
      assert.equal(count, mode === 'backend' ? 10 : 3)
    }
  }
})
