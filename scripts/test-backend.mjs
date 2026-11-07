import { Buffer } from 'node:buffer'
import { spawnSync } from 'node:child_process'
import {
  closeSync,
  copyFileSync,
  mkdtempSync,
  openSync,
  readSync,
  rmSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

// A coding-agent VM may expose its live auth broker to child processes. Tests
// must use their own temporary homes and synthetic brokers instead.
isolateTestCredentials()
delete process.env.CAIRN_TEST_WORKER_FIXTURES
delete process.env.CAIRN_TEST_CODE_WORKER
delete process.env.CAIRN_TEST_CODE_WORKERS
delete process.env.CAIRN_TEST_REUSED_BUILD
const nextest = process.argv[2] === '--nextest'
// The integration fixtures start real workers and databases. Unbounded libtest
// parallelism can exhaust the shared host and make deadline tests unreliable.
// Preserve an explicit caller choice; nextest controls its own concurrency.
if (!nextest && !process.env.RUST_TEST_THREADS)
  process.env.RUST_TEST_THREADS = '4'
const args = process.argv.slice(nextest ? 3 : 2)
const delimiter = args.indexOf('--')
const cargoArgs = delimiter < 0 ? args : args.slice(0, delimiter)
const reuseOptions = new Set(['--archive-file', '--cargo-metadata', '--binaries-metadata', '--workspace-remap', '--target-dir-remap', '--build-dir-remap'])
if (nextest && cargoArgs.some(arg => reuseOptions.has(arg.split('=')[0])))
  process.env.CAIRN_TEST_REUSED_BUILD = '1'
const packages = cargoArgs.flatMap((arg, index) => {
  if (arg === '-p' || arg === '--package')
    return cargoArgs[index + 1] ? [cargoArgs[index + 1]] : []
  if (arg.startsWith('--package='))
    return [arg.slice('--package='.length)]
  return arg.startsWith('-p') && !arg.startsWith('--') ? [arg.slice(2).replace(/^=/, '')] : []
})
const selectsPackage = packages.length > 0

function maySelectPackage(spec, name) {
  const fragment = spec.split('#').at(-1)
  // A source-only spec or a version-only fragment needs Cargo's resolution;
  // conservatively build the fixture in case it names the relevant package.
  if (spec.includes('://') && (!spec.includes('#') || /^\d/.test(fragment)))
    return true
  const pattern = fragment.split(/[@:]/)[0]
  return /[*?[\]]/.test(pattern) ? path.matchesGlob(name, pattern) : pattern === name
}

const valuedBuildOptions = new Set(['--target', '--target-dir', '--config', '--jobs', '--manifest-path', '--features', '-F'])
const buildSwitches = new Set(['--release', '-r', '--offline', '--frozen', '--all-features', '--no-default-features', '--ignore-rust-version'])
const buildArgs = []
for (let i = 0; i < cargoArgs.length; i++) {
  const arg = cargoArgs[i]
  const name = arg.split('=')[0]
  if ((!nextest && /^-j\d/.test(arg)) || /^-F.+/.test(arg) || (!nextest && /^-m.+/.test(arg))) {
    buildArgs.push(arg)
    continue
  }

  if (nextest && ['--profile', '-P', '-j'].includes(name)) {
    if (!arg.includes('='))
      i++
    continue
  }

  const mapped = nextest && name === '--cargo-profile'
    ? '--profile'
    : nextest && name === '--build-jobs' ? '--jobs' : !nextest && name === '-m' ? '--manifest-path' : name
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

const selectsWorkspace = cargoArgs.includes('--workspace') || cargoArgs.includes('--all')
const installationSelected = !selectsPackage || selectsWorkspace
  || packages.some(spec => maySelectPackage(spec, 'cairn-installation'))
const testTargets = cargoArgs.flatMap((arg, index) => {
  if (arg === '--test')
    return cargoArgs[index + 1] ? [cargoArgs[index + 1]] : []
  return arg.startsWith('--test=') ? [arg.slice('--test='.length)] : []
})
const explicitOtherTarget = cargoArgs.some(arg => ['--lib', '--bin', '--bins', '--example', '--examples', '--bench', '--benches', '--doc'].includes(arg.split('=')[0]))
const includesAllTests = cargoArgs.some(arg => ['--tests', '--all-targets'].includes(arg))
const needsWorkerFixture = installationSelected
  && (includesAllTests || (testTargets.length > 0 ? testTargets.some(target => /[*?[\]]/.test(target) ? path.matchesGlob('worker', target) : target === 'worker') : !explicitOtherTarget))
const needsCodeWorker = !selectsPackage || selectsWorkspace
  || packages.some(spec => maySelectPackage(spec, 'cairn-beacon'))

// Inspect bytes instead of executing the fixture: it may target another architecture.
function hasFixtureHooks(binary) {
  const marker = Buffer.from('cairn-worker-fixture-hooks-v1')
  const fd = openSync(binary, 'r')
  const buffer = Buffer.alloc(65536 + marker.length - 1)
  let retained = 0
  try {
    for (;;) {
      const read = readSync(fd, buffer, retained, 65536, null)
      if (read === 0)
        return false
      const length = retained + read
      if (buffer.subarray(0, length).includes(marker))
        return true
      retained = Math.min(marker.length - 1, length)
      buffer.copyWithin(0, length - retained, length)
    }
  }
  finally {
    closeSync(fd)
  }
}

const archivedBinaries = new Set(['--archive-file', '--binaries-metadata'])
const usesArchive = cargoArgs.some(arg => archivedBinaries.has(arg.split('=')[0]))
const usesCargoMetadata = cargoArgs.some(arg => arg.split('=')[0] === '--cargo-metadata')
if (nextest && (usesArchive || (usesCargoMetadata && (needsWorkerFixture || needsCodeWorker)))) {
  console.error('Nextest archived or reused binaries cannot be paired safely with fixtures built from this checkout; run these tests from a matching fresh build.')
  process.exit(2)
}

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
  const fixtures = records.filter(record => record.reason === 'compiler-artifact' && record.target?.name === 'worker_fixture' && record.executable)
  if (fixtures.length === 0)
    throw new Error('Cargo did not produce the worker fixture')
  // Cargo test may rebuild the default example at its usual path. Preserve the
  // feature-enabled binary for this launcher invocation before that can happen.
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-worker-fixture-'))
  process.on('exit', () => rmSync(directory, { recursive: true, force: true }))
  const artifacts = new Map()
  for (const fixture of fixtures) {
    const source = path.resolve(fixture.executable)
    const target = path.dirname(path.dirname(source))
    if (artifacts.has(target))
      continue
    const binary = path.join(directory, `worker_fixture-${artifacts.size}${path.extname(source)}`)
    copyFileSync(source, binary)
    if (!hasFixtureHooks(binary))
      throw new Error('Copied worker fixture lacks test hooks; another Cargo build may have replaced it')
    artifacts.set(target, binary)
  }

  process.env.CAIRN_TEST_WORKER_FIXTURES = JSON.stringify([...artifacts].map(([target, binary]) => ({ target, binary })))
}

// Beacon's real MCP relay tests also execute the installation's isolated code
// worker. Build it with the same profile flags before tests start, so a
// nested Cargo invocation cannot overwrite the binary with another profile.
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
  const workers = records.filter(record => record.reason === 'compiler-artifact' && record.target?.name === 'cairn' && record.executable)
  if (workers.length === 0)
    throw new Error('Cargo did not produce the Cairn code worker')
  const targets = new Map()
  for (const worker of workers) {
    const binary = path.resolve(worker.executable)
    targets.set(path.dirname(binary), binary)
  }

  process.env.CAIRN_TEST_CODE_WORKERS = JSON.stringify([...targets].map(([target, binary]) => ({ target, binary })))
  process.env.CAIRN_TEST_CODE_WORKER = path.resolve(workers[0].executable)
}

const result = spawnSync('cargo', [...(nextest ? ['nextest', 'run'] : ['test']), '--locked', ...(selectsPackage || cargoArgs.includes('--workspace') || cargoArgs.includes('--all') ? [] : ['--workspace']), ...args], { stdio: 'inherit' })
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
