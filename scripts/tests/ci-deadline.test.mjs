import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { it } from 'vitest'
import { parse } from 'yaml'
import { instrumentBackend } from '../backend-ci-phase-overlay.mjs'
import { observeCommand } from '../ci-deadline.mjs'
import { selectJobClock } from '../ci-diagnostic-source.mjs'

function temporary(t) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-ci-deadline-'))
  t.onTestFinished(() => rmSync(directory, { recursive: true, force: true }))
  return directory
}

function live(pid) {
  try {
    return readFileSync(`/proc/${pid}/stat`, 'utf8').split(') ')[1][0] !== 'Z'
  }
  catch {
    return false
  }
}

it('captures a blocked command before the job limit and kills only its owned subtree', async (t) => {
  const directory = temporary(t)
  const pids = path.join(directory, 'pids.json')
  const secret = 'private-environment-and-argument-must-not-be-observed'
  const child = `process.on('SIGTERM',()=>{});setInterval(()=>{},1000)`
  const command = `
    const fs=require('node:fs');const {spawn}=require('node:child_process');
    process.on('SIGTERM',()=>{});
    const child=spawn(process.execPath,['-e',${JSON.stringify(child)},${JSON.stringify(secret)}],{stdio:'ignore'});
    fs.writeFileSync(${JSON.stringify(pids)},JSON.stringify([process.pid,child.pid]));
    setInterval(()=>{},1000);
  `
  const jobStartedAt = Date.now()
  const status = await observeCommand({
    command: process.execPath,
    args: ['-e', command, secret],
    output: directory,
    jobStartedAt,
    jobMinutes: 0.05,
    reserveSeconds: 2,
    sampleMs: 40,
    graceMs: 80,
  })
  assert.equal(status, 124)
  assert.ok(Date.now() < jobStartedAt + 3000, 'evidence and cleanup must retain time inside the job limit')
  const proof = readFileSync(path.join(directory, 'latest.json'), 'utf8')
  assert.ok(!proof.includes(secret))
  const data = JSON.parse(proof)
  assert.equal(data.timedOut, true)
  assert.ok(data.samples.some(sample => sample.reason === 'deadline'))
  assert.ok(data.samples.some(sample => sample.processCount >= 2))
  assert.ok(data.samples.length <= 12)
  assert.ok(data.samples.every(sample => sample.processes.length <= 16))
  for (const pid of JSON.parse(readFileSync(pids, 'utf8')))
    assert.equal(live(pid), false, `owned process ${pid} survived`)
  assert.equal(live(process.pid), true, 'observer caller must remain alive')
})

it('preserves a normal command failure instead of counting it as a diagnostic pass', async (t) => {
  const output = temporary(t)
  assert.equal(await observeCommand({
    command: process.execPath,
    args: ['-e', 'process.exit(7)'],
    output,
    jobStartedAt: Date.now(),
    jobMinutes: 0.1,
    reserveSeconds: 0,
    sampleMs: 40,
    graceMs: 50,
  }), 7)
  assert.equal(JSON.parse(readFileSync(path.join(output, 'latest.json'), 'utf8')).timedOut, false)
})

it('does not start work after the evidence reserve has begun', async (t) => {
  const output = temporary(t)
  const marker = path.join(output, 'not-started')
  const status = await observeCommand({
    command: process.execPath,
    args: ['-e', `require('node:fs').writeFileSync(${JSON.stringify(marker)},'wrong')`],
    output,
    jobStartedAt: Date.now() - 4000,
    jobMinutes: 0.05,
    reserveSeconds: 0,
  })
  assert.equal(status, 124)
  assert.equal(existsSync(marker), false)
})

it('backend overlay keeps the exact original launcher and refuses ambiguous sources', () => {
  const source = readFileSync(new URL('../test-backend.mjs', import.meta.url), 'utf8')
  const observed = instrumentBackend(source)
  assert.ok(observed.includes('recordCIPhase(\'worker-fixture-build\')'))
  assert.ok(observed.includes('recordCIPhase(\'code-worker-build\')'))
  assert.ok(observed.includes('recordCIPhase(\'backend-tests\')'))
  assert.equal(observed.slice(observed.indexOf('import { Buffer }')).replace(/^ {0,2}recordCIPhase\('[a-z-]+'\)\n/gm, ''), source)
  assert.throws(() => instrumentBackend(observed))
  assert.throws(() => instrumentBackend(source + source))
})

it('overlaid launcher writes parseable coarse phases without changing its command', (t) => {
  const directory = temporary(t)
  const phase = path.join(directory, 'phase.json')
  const source = `
const needsWorkerFixture=false, needsCodeWorker=false;
function spawnSync(command) { if(command!=='cargo')throw Error('Command changed'); return {status:0}; }
if (needsWorkerFixture) {
}
if (needsCodeWorker) {
}
const result = spawnSync('cargo', ['test']);
`
  const result = spawnSync(process.execPath, ['--input-type=module'], {
    input: instrumentBackend(source),
    encoding: 'utf8',
    env: { ...process.env, CAIRN_CI_PHASE_FILE: phase },
  })
  assert.equal(result.status, 0, result.stderr)
  assert.deepEqual(JSON.parse(readFileSync(phase, 'utf8')), { phase: 'backend-tests' })
})

it('uses actual GitHub job start, including service/setup time, or refuses ambiguity', () => {
  const now = Date.now()
  const startedAt = now - 180000
  const job = {
    id: 123,
    status: 'in_progress',
    runner_name: 'owned-runner',
    started_at: new Date(startedAt).toISOString(),
  }
  assert.deepEqual(selectJobClock({ total_count: 1, jobs: [job] }, 'owned-runner', now), { jobId: 123, startedAt })
  for (const jobs of [[], [job, job], [{ ...job, status: 'completed' }]])
    assert.throws(() => selectJobClock({ total_count: jobs.length, jobs }, 'owned-runner', now))
  assert.throws(() => selectJobClock({ total_count: 101, jobs: [job] }, 'owned-runner', now))
  assert.throws(() => selectJobClock({ total_count: 1, jobs: [{ ...job, started_at: new Date(now + 1000).toISOString() }] }, 'owned-runner', now))
})

it('does not return success when the command exits before an owned descendant', async (t) => {
  const output = temporary(t)
  const file = path.join(output, 'orphan.pid')
  const code = `const {spawn}=require('node:child_process');const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'});require('node:fs').writeFileSync(${JSON.stringify(file)},''+child.pid);child.unref()`
  let pid
  try {
    const status = await observeCommand({
      command: process.execPath,
      args: ['-e', code],
      output,
      jobStartedAt: Date.now(),
      jobMinutes: 0.05,
      reserveSeconds: 0,
      sampleMs: 1000,
      graceMs: 50,
    })
    pid = Number(readFileSync(file, 'utf8'))
    assert.notEqual(status, 0)
    assert.equal(live(pid), false)
  }
  finally {
    pid ??= existsSync(file) ? Number(readFileSync(file, 'utf8')) : undefined
    if (pid && live(pid))
      process.kill(pid, 'SIGKILL')
  }
})

it('reports incomplete process inventory as UNKNOWN instead of succeeding', async (t) => {
  const output = temporary(t)
  const status = await observeCommand({
    command: process.execPath,
    args: ['-e', 'process.exit(0)'],
    output,
    jobStartedAt: Date.now(),
    jobMinutes: 0.05,
    reserveSeconds: 0,
    sampleMs: 40,
    graceMs: 50,
    inventoryLimit: 1,
  })
  assert.notEqual(status, 0)
  const proof = JSON.parse(readFileSync(path.join(output, 'latest.json'), 'utf8'))
  assert.equal(proof.inventoryComplete, false)
  assert.equal(proof.cleanupUnknown, true)
})

it('adopts and closes an escaped stdout owner within the publication reserve', async (t) => {
  const output = temporary(t)
  const file = path.join(output, 'escaped.pid')
  const code = `const {spawn}=require('node:child_process');const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{detached:true,stdio:['ignore','inherit','inherit']});require('node:fs').writeFileSync(${JSON.stringify(file)},''+child.pid);child.unref()`
  let pid
  const started = Date.now()
  try {
    const status = await observeCommand({
      command: process.execPath,
      args: ['-e', code],
      output,
      jobStartedAt: started,
      jobMinutes: 0.05,
      reserveSeconds: 0,
      sampleMs: 1000,
      graceMs: 50,
      closeGraceMs: 50,
    })
    pid = Number(readFileSync(file, 'utf8'))
    assert.notEqual(status, 0)
    assert.ok(Date.now() - started < 1000, 'pipe/close wait must retain its evidence-upload reserve')
    const proof = JSON.parse(readFileSync(path.join(output, 'latest.json'), 'utf8'))
    assert.equal(proof.containmentReady, true)
    assert.equal(proof.cleanupUnknown, false)
    assert.equal(live(pid), false)
  }
  finally {
    pid ??= existsSync(file) ? Number(readFileSync(file, 'utf8')) : undefined
    if (pid && live(pid))
      process.kill(pid, 'SIGKILL')
  }
})

it('adopts a fast setsid daemon with closed stdio before its parent disappears', async (t) => {
  const output = temporary(t)
  const file = path.join(output, 'daemon.json')
  const code = `const {spawn}=require('node:child_process');const fs=require('node:fs');const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{detached:true,stdio:'ignore'});const fields=fs.readFileSync('/proc/'+child.pid+'/stat','utf8').split(') ')[1].split(/\\s+/);fs.writeFileSync(${JSON.stringify(file)},JSON.stringify({pid:child.pid,start:fields[19]}));child.unref()`
  let daemon
  try {
    const status = await observeCommand({
      command: process.execPath,
      args: ['-e', code],
      output,
      jobStartedAt: Date.now(),
      jobMinutes: 0.05,
      reserveSeconds: 0,
      sampleMs: 1000,
      graceMs: 50,
      closeGraceMs: 100,
    })
    daemon = JSON.parse(readFileSync(file, 'utf8'))
    assert.match(daemon.start, /^\d+$/)
    assert.notEqual(status, 0)
    assert.equal(live(daemon.pid), false)
    const proof = JSON.parse(readFileSync(path.join(output, 'latest.json'), 'utf8'))
    assert.equal(proof.containmentReady, true)
    assert.equal(proof.cleanupUnknown, false)
    assert.ok(proof.samples.some(sample => sample.processes.some(p => p.pid === daemon.pid && p.startTicks === daemon.start)))
  }
  finally {
    daemon ??= existsSync(file) ? JSON.parse(readFileSync(file, 'utf8')) : undefined
    if (daemon && live(daemon.pid)) {
      const fields = readFileSync(`/proc/${daemon.pid}/stat`, 'utf8').split(') ')[1].split(/\s+/)
      if (fields[19] === daemon.start)
        process.kill(daemon.pid, 'SIGKILL')
    }
  }
})

it('rejects an independent Plan storage request before reading CI metadata or fetching sources', () => {
  const file = new URL('../ci-diagnostic-source.mjs', import.meta.url)
  const result = spawnSync(process.execPath, [file.pathname, 'backend'], {
    encoding: 'utf8',
    env: {
      ...process.env,
      BACKEND_ONLY: 'true',
      VALIDATION_ONLY: 'true',
      BACKEND_QUALIFICATION: 'none',
      DIAGNOSTIC_CANDIDATE: '358461be03f947ff168950d0a85999e5ad5f9274',
      PLAN_STORAGE_REFERENCE: '06829d1b5f3d7accb9791c09d49522e7e2d2ab25',
    },
  })
  assert.notEqual(result.status, 0)
  assert.match(result.stderr, /Diagnostics require/)
})

it('executes the actual reference verification after the pair builder created its evidence directory', (t) => {
  const directory = temporary(t)
  const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  const step = workflow.jobs['native-runtime'].steps.find(value => value.name === 'Verify the exact reference binaries')
  const code = step.run.split('python3 - <<\'PYREF\'\n')[1].split('\nPYREF')[0]
  const checksums = {}
  for (const name of ['cairn', 'examples/native_storage_qualifier']) {
    const file = path.join(directory, 'reference-native', name)
    mkdirSync(path.dirname(file), { recursive: true })
    const data = `${name}-tiny-fixture`
    writeFileSync(file, data)
    checksums[name] = createHash('sha256').update(data).digest('hex')
  }

  const metadata = {
    headSha: '06829d1b5f3d7accb9791c09d49522e7e2d2ab25',
    image: 'fixture-image',
    rust: '1.97.1',
    features: 'default (ublk required)',
    profile: 'development, debug=0, incremental=0',
    sha256: checksums,
  }
  const evidence = path.join(directory, 'native-storage-evidence/reference')
  mkdirSync(evidence, { recursive: true })
  writeFileSync(path.join(evidence, 'revision.json'), JSON.stringify(metadata))
  writeFileSync(path.join(directory, 'reference-native/revision.json'), JSON.stringify(metadata))
  const options = { cwd: directory, encoding: 'utf8', env: { ...process.env, REFERENCE_SHA: metadata.headSha, ImageVersion: metadata.image } }
  const result = spawnSync('python3', ['-B', '-c', code], options)
  assert.equal(result.status, 0, result.stderr)
  assert.deepEqual(JSON.parse(readFileSync(path.join(evidence, 'revision.json'), 'utf8')), metadata)
  writeFileSync(path.join(directory, 'reference-native/cairn'), 'changed')
  assert.notEqual(spawnSync('python3', ['-B', '-c', code], options).status, 0)
  assert.deepEqual(JSON.parse(readFileSync(path.join(evidence, 'revision.json'), 'utf8')), metadata)
})

it('keeps actual archive argv at HEAD normally and uses the exact candidate only when selected', (t) => {
  const directory = temporary(t)
  const bin = path.join(directory, 'bin')
  mkdirSync(bin)
  const record = path.join(directory, 'git-argv.json')
  writeFileSync(path.join(bin, 'git'), '#!/usr/bin/env node\nrequire(\'node:fs\').writeFileSync(process.env.CI_TEST_GIT_ARGS,JSON.stringify(process.argv.slice(2)))\n', { mode: 0o755 })
  writeFileSync(path.join(bin, 'tar'), '#!/bin/sh\ncat >/dev/null\n', { mode: 0o755 })
  const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  const step = workflow.jobs['native-runtime'].steps.find(value => value.name === 'Qualify local disks, ublk, systemd ownership and crash recovery')
  const code = step.run.split('\n').find(line => line.startsWith('git archive '))
  for (const selected of ['', '358461be03f947ff168950d0a85999e5ad5f9274']) {
    const result = spawnSync('bash', ['-e', '-o', 'pipefail', '-c', code], {
      encoding: 'utf8',
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        DIAGNOSTIC_CANDIDATE: selected,
        candidate_fixture: directory,
        CI_TEST_GIT_ARGS: record,
      },
    })
    assert.equal(result.status, 0, result.stderr)
    assert.deepEqual(JSON.parse(readFileSync(record, 'utf8')), ['archive', selected || 'HEAD', 'scripts/tests', 'deploy/nodes/native-host.py'])
  }
})

it('verifies the pinned candidate qualifier before local preparation tests', (t) => {
  const directory = temporary(t)
  const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
  const step = workflow.jobs['native-runtime'].steps.find(value => value.name === 'Verify the exact reference binaries')
  const code = step.run.split('python3 - <<\'PYREF\'\n')[1].split('\nPYREF')[0]
  const revisions = { reference: '06829d1b5f3d7accb9791c09d49522e7e2d2ab25', candidate: '358461be03f947ff168950d0a85999e5ad5f9274' }
  for (const role of ['reference', 'candidate']) {
    const destination = role === 'reference' ? 'reference-native' : 'target/debug'
    const checksums = {}
    for (const name of ['cairn', 'examples/native_storage_qualifier']) {
      const file = path.join(directory, destination, name)
      mkdirSync(path.dirname(file), { recursive: true })
      const data = `${role}/${name}-tiny-fixture`
      writeFileSync(file, data)
      checksums[name] = createHash('sha256').update(data).digest('hex')
    }

    writeFileSync(path.join(directory, destination, 'revision.json'), JSON.stringify({
      headSha: revisions[role],
      image: 'fixture-image',
      rust: '1.97.1',
      features: 'default (ublk required)',
      profile: 'development, debug=0, incremental=0',
      sha256: checksums,
    }))
  }

  mkdirSync(path.join(directory, 'native-diagnostics'))
  writeFileSync(path.join(directory, 'native-diagnostics/source.json'), JSON.stringify({ revisions }))
  const options = {
    cwd: directory,
    encoding: 'utf8',
    env: {
      ...process.env,
      CI_DIAGNOSTIC_SOURCE: path.join(directory, 'pinned-candidate'),
      REFERENCE_SHA: revisions.reference,
      ImageVersion: 'fixture-image',
    },
  }
  const verify = () => spawnSync('python3', ['-B', '-c', code], options)
  const success = verify()
  assert.equal(success.status, 0, success.stderr)
  const metadataFile = path.join(directory, 'target/debug/revision.json')
  const metadata = JSON.parse(readFileSync(metadataFile, 'utf8'))
  writeFileSync(metadataFile, JSON.stringify({ ...metadata, headSha: '41a323d0a3ff74d1486358dbef8611cc1c42fa6d' }))
  assert.notEqual(verify().status, 0, 'observer commit must not replace pinned candidate identity')
  writeFileSync(metadataFile, JSON.stringify(metadata))
  writeFileSync(path.join(directory, 'target/debug/examples/native_storage_qualifier'), 'unverified-qualifier')
  assert.notEqual(verify().status, 0, 'unverified qualifier bytes must not reach the consumer')
})
