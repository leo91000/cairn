import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

// A coding-agent VM may expose its live auth broker to child processes. Tests
// must use their own temporary homes and synthetic brokers instead.
isolateTestCredentials()
const args = process.argv.slice(2)
const selectsPackage = args.some(arg => arg === '-p' || arg === '--package' || arg.startsWith('--package='))
// Beacon's real MCP relay tests also execute the installation's isolated code
// worker. Build it with the same feature/profile flags before tests start, so a
// nested Cargo invocation cannot overwrite the binary with another feature set.
const needsCodeWorker = !selectsPackage || args.includes('--workspace')
  || args.includes('cairn-beacon') || args.includes('--package=cairn-beacon')
if (needsCodeWorker) {
  const delimiter = args.indexOf('--')
  const cargoArgs = delimiter < 0 ? args : args.slice(0, delimiter)
  const valued = new Set(['--features', '-F', '--target', '--target-dir', '--profile', '--config', '--jobs', '-j'])
  const switches = new Set(['--all-features', '--no-default-features', '--release', '--offline', '--frozen'])
  const buildArgs = []
  for (let i = 0; i < cargoArgs.length; i++) {
    const arg = cargoArgs[i]
    const name = arg.split('=')[0]
    if (valued.has(name)) {
      buildArgs.push(arg)
      if (!arg.includes('=') && cargoArgs[i + 1])
        buildArgs.push(cargoArgs[++i])
    }
    else if (switches.has(arg)) {
      buildArgs.push(arg)
    }
  }

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

const result = spawnSync('cargo', ['test', '--locked', ...(selectsPackage ? [] : ['--workspace']), ...args], { stdio: 'inherit' })
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
