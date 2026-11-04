import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

// Completed backend qualification allocated 22.49 GiB beyond its initial state.
// Keep another 9.5 GiB available for temporary work and growth; this is not a peak bound.
export const REQUIRED_FREE_BYTES = 32n * 1024n ** 3n

export const UNUSED_SDK_DIRECTORIES = Object.freeze([
  '/usr/local/lib/android',
  '/usr/share/dotnet',
  '/usr/share/swift',
])

const jobs = new Set(['backend', 'cli-qualification'])
const buildTools = ['cc', 'c++', 'clang', 'pkg-config', 'node', 'cargo', 'rustc', 'python3']
const nativeInputs = ['CC', 'CXX', 'AR', 'RUSTC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'LIBCLANG_PATH', 'CLANG_PATH', 'LLVM_CONFIG_PATH', 'CMAKE_TOOLCHAIN_FILE', 'CMAKE_PREFIX_PATH', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS']

function contains(directory, location) {
  return location === directory || location.startsWith(`${directory}${path.sep}`)
}

export function prepareRustRunner(environment, dependencies = {}) {
  const filesystem = dependencies.filesystem ?? fs
  const execute = dependencies.execute ?? execFileSync
  const user = (dependencies.userInfo ?? os.userInfo)()
  const platform = dependencies.platform ?? process.platform
  const now = dependencies.now ?? Date.now
  const deadline = now() + 60000
  const remaining = () => {
    const milliseconds = deadline - now()
    if (milliseconds <= 0)
      throw new Error('Rust SDK reclamation exceeded its 60-second preparation budget.')
    return milliseconds
  }

  if (platform !== 'linux' || user.username !== 'runner'
    || environment.GITHUB_ACTIONS !== 'true' || environment.RUNNER_ENVIRONMENT !== 'github-hosted'
    || environment.RUNNER_OS !== 'Linux' || !/^ubuntu\d+$/.test(environment.ImageOS ?? '')
    || !/^\d{8}\.\d+\.\d+$/.test(environment.ImageVersion ?? '') || !jobs.has(environment.GITHUB_JOB)) {
    throw new Error('Rust disk preparation requires an approved GitHub-hosted Ubuntu test job and the runner account.')
  }

  const locations = [environment.GITHUB_WORKSPACE, environment.RUNNER_TEMP, environment.HOME, environment.CARGO_HOME || (environment.HOME && path.join(environment.HOME, '.cargo'))]
  if (locations.some(location => !location || !path.isAbsolute(location)) || environment.HOME !== user.homedir)
    throw new Error('Rust disk preparation requires the runner account and absolute build locations.')

  const canonical = location => filesystem.realpathSync(location)
  const protectedDirectories = locations.map(canonical)
  const workspace = protectedDirectories[0]
  const device = filesystem.statSync(workspace).dev
  if (protectedDirectories.some(location => filesystem.statSync(location).dev !== device))
    throw new Error('Rust build, temporary and Cargo directories must share the measured filesystem.')
  if (environment.CARGO_TARGET_DIR)
    throw new Error('Rust disk preparation does not support a custom Cargo target directory.')

  for (const directory of UNUSED_SDK_DIRECTORIES) {
    if (protectedDirectories.some(location => contains(directory, location) || contains(location, directory)))
      throw new Error('An unused SDK directory overlaps a protected build location.')
    if (nativeInputs.some(name => environment[name] === directory || environment[name]?.includes(`${directory}/`)))
      throw new Error(`A configured native build input uses the SDK directory: ${directory}`)
  }

  const toolLocations = execute('which', buildTools, { encoding: 'utf8', timeout: Math.min(10000, remaining()), stdio: ['ignore', 'pipe', 'pipe'] })
    .trim()
    .split('\n')
    .map(canonical)
  if (toolLocations.length !== buildTools.length)
    throw new Error('Could not identify the active native build tools before SDK reclamation.')

  const available = () => {
    const stat = filesystem.statfsSync(workspace, { bigint: true })
    return stat.bavail * stat.bsize
  }

  const before = available()
  const directories = []
  for (const directory of UNUSED_SDK_DIRECTORIES) {
    let stat
    try {
      stat = filesystem.lstatSync(directory)
    }
    catch (error) {
      if (error.code !== 'ENOENT')
        throw error
      directories.push({ path: directory, status: 'absent' })
      continue
    }

    if (!stat.isDirectory() || stat.isSymbolicLink() || stat.uid !== 0 || canonical(directory) !== directory)
      throw new Error(`Refusing an unexpected preinstalled SDK directory: ${directory}`)
    if (stat.dev !== device) {
      directories.push({ path: directory, status: 'different-filesystem' })
      continue
    }

    if (toolLocations.some(location => contains(directory, location)))
      throw new Error(`An active native build tool uses the SDK directory: ${directory}`)

    execute('sudo', ['-n', 'rm', '--recursive', '--force', '--one-file-system', '--', directory], { stdio: 'inherit', timeout: remaining() })
    directories.push({ path: directory, status: 'removed' })
  }

  const after = available()
  const report = {
    schemaVersion: 1,
    job: environment.GITHUB_JOB,
    imageOS: environment.ImageOS,
    imageVersion: environment.ImageVersion,
    beforeBytes: `${before}`,
    afterBytes: `${after}`,
    requiredBytes: `${REQUIRED_FREE_BYTES}`,
    directories,
  }
  if (after < REQUIRED_FREE_BYTES) {
    const error = new Error(`Rust test compilation requires ${REQUIRED_FREE_BYTES} available bytes; only ${after} remain after unused SDK reclamation.`)
    error.report = report
    throw error
  }

  return report
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.argv.length !== 2)
      throw new Error('Usage: node scripts/prepare-rust-runner.mjs (no arguments)')
    console.log(JSON.stringify(prepareRustRunner(process.env)))
  }
  catch (error) {
    if (error.report)
      console.log(JSON.stringify(error.report))
    console.error(error.message)
    process.exitCode = 1
  }
}
