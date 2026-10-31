import { spawnSync } from 'node:child_process'
import { copyFileSync, mkdtempSync, rmSync } from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

// A coding-agent VM may expose its live auth broker to child processes. Tests
// must use their own temporary homes and synthetic brokers instead.
isolateTestCredentials()
delete process.env.CAIRN_TEST_WORKER_FIXTURE
const nextest = process.argv[2] === '--nextest'
const args = process.argv.slice(nextest ? 3 : 2)
const selectsPackage = args.some(arg => arg === '-p' || arg === '--package' || arg.startsWith('--package='))
const delimiter = args.indexOf('--')
const cargoArgs = delimiter < 0 ? args : args.slice(0, delimiter)
const valuedBuildOptions = new Set(['--target', '--target-dir', '--config', '--jobs'])
const buildSwitches = new Set(['--release', '-r', '--offline', '--frozen'])
const buildArgs = []
for (let i = 0; i < cargoArgs.length; i++) {
  const arg = cargoArgs[i]
  const name = arg.split('=')[0]
  if (nextest && ['--profile', '-P', '-j'].includes(name)) {
    if (!arg.includes('='))
      i++
    continue
  }

  const mapped = nextest && name === '--cargo-profile'
    ? '--profile'
    : nextest && name === '--build-jobs' ? '--jobs' : name
  if (valuedBuildOptions.has(mapped) || mapped === '--profile' || (!nextest && mapped === '-j')) {
    if (arg.includes('=')) {
      buildArgs.push(`${mapped}=${arg.slice(arg.indexOf('=') + 1)}`)
    }
    else {
      buildArgs.push(mapped)
      if (cargoArgs[i + 1])
        buildArgs.push(cargoArgs[++i])
    }
  }
  else if (buildSwitches.has(arg)) {
    buildArgs.push(arg)
  }
}

const installationSelected = !selectsPackage || args.includes('--workspace')
  || args.includes('cairn-installation') || args.includes('--package=cairn-installation')
const testTargets = cargoArgs.flatMap((arg, index) => {
  if (arg === '--test')
    return cargoArgs[index + 1] ? [cargoArgs[index + 1]] : []
  return arg.startsWith('--test=') ? [arg.slice('--test='.length)] : []
})
const explicitOtherTarget = cargoArgs.some(arg => ['--lib', '--bin', '--bins', '--example', '--examples', '--doc'].includes(arg))
const needsWorkerFixture = installationSelected
  && (testTargets.length > 0 ? testTargets.includes('worker') : !explicitOtherTarget)
if (needsWorkerFixture) {
  const build = spawnSync('cargo', [
    'build',
    '--locked',
    '-p',
    'cairn-installation',
    '--example',
    'worker_fixture',
    '--features',
    'worker-fixture-hooks',
    '--message-format=json',
    ...buildArgs,
  ], { stdio: ['inherit', 'pipe', 'inherit'], encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
  if (build.error)
    throw build.error
  const records = build.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line))
  for (const record of records) {
    if (record.reason === 'compiler-message' && record.message?.rendered)
      process.stderr.write(record.message.rendered)
  }

  if (build.status !== 0)
    process.exit(build.status ?? 1)
  const fixture = records.find(record => record.reason === 'compiler-artifact' && record.target?.name === 'worker_fixture' && record.executable)
  if (!fixture)
    throw new Error('Cargo did not produce the worker fixture')
  // Cargo test may rebuild the default example at its usual path. Preserve the
  // feature-enabled binary for this launcher invocation before that can happen.
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-worker-fixture-'))
  process.on('exit', () => rmSync(directory, { recursive: true, force: true }))
  const binary = path.join(directory, 'worker_fixture')
  copyFileSync(fixture.executable, binary)
  process.env.CAIRN_TEST_WORKER_FIXTURE = binary
}

// Beacon's real MCP relay tests also execute the installation's isolated code
// worker. Build it with the same profile flags before tests start, so a
// nested Cargo invocation cannot overwrite the binary with another profile.
const needsCodeWorker = !selectsPackage || args.includes('--workspace')
  || args.includes('cairn-beacon') || args.includes('--package=cairn-beacon')
if (needsCodeWorker) {
  const build = spawnSync('cargo', [
    'build',
    '--locked',
    '-p',
    'cairn-installation',
    '--bin',
    'cairn',
    '--message-format=json',
    ...buildArgs,
  ], { stdio: ['inherit', 'pipe', 'inherit'], encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 })
  if (build.error)
    throw build.error
  const records = build.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line))
  for (const record of records) {
    if (record.reason === 'compiler-message' && record.message?.rendered)
      process.stderr.write(record.message.rendered)
  }

  if (build.status !== 0)
    process.exit(build.status ?? 1)
  const worker = records.find(record => record.reason === 'compiler-artifact' && record.target?.name === 'cairn' && record.executable)
  if (!worker)
    throw new Error('Cargo did not produce the Cairn code worker')
  process.env.CAIRN_TEST_CODE_WORKER = worker.executable
}

const result = spawnSync('cargo', [...(nextest ? ['nextest', 'run'] : ['test']), '--locked', ...(selectsPackage ? [] : ['--workspace']), ...args], { stdio: 'inherit' })
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
