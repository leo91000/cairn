import { execFileSync, spawnSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { parse } from 'yaml'
import { REQUIRED_FREE_BYTES } from './prepare-rust-runner.mjs'

const mode = process.argv[2]
const job = mode === 'backend' ? 'backend' : mode === 'cli' ? 'cli-qualification' : ''
if (!job || process.env.GITHUB_JOB !== job || process.platform !== 'linux'
  || process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted'
  || os.userInfo().username !== 'runner' || !path.isAbsolute(process.env.RUNNER_TEMP || '')) {
  throw new Error('Capacity qualification is restricted to its disposable hosted job.')
}

function available() {
  const stat = fs.statfsSync(process.cwd(), { bigint: true })
  return stat.bavail * stat.bsize
}

if (mode === 'backend' && fs.existsSync('target/debug'))
  throw new Error('Capacity qualification requires a cold workspace build directory.')

const directory = path.join(process.env.RUNNER_TEMP, 'cairn-rust-disk-proof')
fs.mkdirSync(directory, { recursive: true, mode: 0o700 })
const reservation = path.join(process.env.RUNNER_TEMP, `cairn-rust-disk-proof-${randomUUID()}.reserve`)
const descriptor = fs.openSync(reservation, 'wx', 0o600)
const before = available()
const lowCapacityBytes = 15913492480n
const retained = REQUIRED_FREE_BYTES + 64n * 1024n ** 2n
let bytes = before > lowCapacityBytes ? before - lowCapacityBytes : 0n
const deadline = Date.now() + 25 * 60 * 1000

function run(command, args) {
  const remaining = deadline - Date.now()
  if (remaining <= 0)
    throw new Error('The capacity proof exhausted its compilation budget.')
  console.log(JSON.stringify({ command, args }))
  const result = spawnSync(command, args, { stdio: 'inherit', timeout: remaining })
  if (result.error)
    throw result.error
  if (result.status !== 0)
    throw new Error(`Capacity proof command failed: ${command} (${result.signal || result.status}).`)
}

try {
  if (bytes > 0) {
    // Child fd 3 refers to our still-open O_EXCL file, not an arbitrary pathname.
    execFileSync('fallocate', ['--length', `${bytes}`, '/proc/self/fd/3'], {
      stdio: ['ignore', 'inherit', 'inherit', descriptor],
      timeout: 60000,
    })
  }

  const beforePreparation = available()
  if (beforePreparation < lowCapacityBytes || beforePreparation > lowCapacityBytes + 128n * 1024n ** 2n)
    throw new Error('Could not establish the observed low initial capacity.')
  console.log(JSON.stringify({
    phase: 'low-capacity-start',
    mode,
    availableBytes: `${beforePreparation}`,
    reservationBytes: `${bytes}`,
  }))
  const action = parse(fs.readFileSync('.github/actions/prepare-rust-runner/action.yaml', 'utf8'))
  if (action.runs.using !== 'composite' || action.runs.steps.length !== 1 || action.runs.steps[0].shell !== 'bash')
    throw new Error('The capacity proof requires the unchanged production preparation action.')
  run('bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', action.runs.steps[0].run])
  const afterPreparation = available()
  if (afterPreparation < REQUIRED_FREE_BYTES)
    throw new Error('Production preparation did not admit this low-capacity runner.')
  const additional = afterPreparation > retained ? afterPreparation - retained : 0n
  if (additional > 0n) {
    bytes += additional
    execFileSync('fallocate', ['--length', `${bytes}`, '/proc/self/fd/3'], {
      stdio: ['ignore', 'inherit', 'inherit', descriptor],
      timeout: 60000,
    })
  }

  const atStart = available()
  if (atStart < REQUIRED_FREE_BYTES || atStart > REQUIRED_FREE_BYTES + 128n * 1024n ** 2n)
    throw new Error('Could not establish the controlled 32 GiB build capacity.')
  console.log(JSON.stringify({
    phase: 'bounded-start',
    mode,
    availableBytes: `${atStart}`,
    reservationBytes: `${bytes}`,
  }))

  if (mode === 'backend') {
    run(process.execPath, ['scripts/test-backend.mjs', '--nextest', '--test-threads', '8', '--color', 'never', '--status-level', 'pass', '--no-run'])
    run(process.execPath, ['scripts/test-backend.mjs', '-p', 'cairn-installation', '--lib', '--no-run'])
  }
  else {
    const workflow = parse(fs.readFileSync('.github/workflows/ci.yaml', 'utf8'))
    const names = ['Qualify the resolved OpenCode release against its native adapter', 'Qualify the resolved Claude Code release against its native adapter']
    for (const name of names) {
      const step = workflow.jobs['cli-qualification'].steps.find(step => step.name === name)
      if (!step)
        throw new Error(`Missing unchanged qualification: ${name}`)
      run('bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', step.run])
    }
  }

  const report = {
    mode,
    head: process.env.GITHUB_SHA,
    helperBlob: execFileSync('git', ['hash-object', 'scripts/prepare-rust-runner.mjs'], { encoding: 'utf8' }).trim(),
    rust: execFileSync('rustc', ['--version'], { encoding: 'utf8' }).trim(),
    imageVersion: process.env.ImageVersion,
    cpu: os.cpus()[0]?.model,
    cpus: os.cpus().length,
    memoryBytes: os.totalmem(),
    availableBeforeReservation: `${before}`,
    availableBeforePreparation: `${beforePreparation}`,
    availableAfterPreparation: `${afterPreparation}`,
    reservationBytes: `${bytes}`,
    availableAtBuildStart: `${atStart}`,
    availableAfterBuilds: `${available()}`,
    targetAllocatedBytes: execFileSync('du', ['-x', '--summarize', '-B1', '--', 'target/debug'], { encoding: 'utf8' }).trim().split(/\s/)[0],
    scope: mode === 'backend' ? 'Cold compilation/allocation only: no backend test verdict or peak bound.' : 'Unchanged resolved CLI qualification under controlled available capacity.',
  }
  fs.writeFileSync(path.join(directory, `${mode}.json`), `${JSON.stringify(report, null, 2)}\n`, { flag: 'wx', mode: 0o600 })
  console.log(JSON.stringify(report))
}
finally {
  fs.closeSync(descriptor)
  fs.unlinkSync(reservation)
}
