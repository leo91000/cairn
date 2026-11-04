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
const nativeInputs = new Set([
  'CC',
  'CXX',
  'AR',
  'ARFLAGS',
  'LD',
  'RANLIB',
  'NM',
  'STRIP',
  'OBJCOPY',
  'OBJDUMP',
  'CFLAGS',
  'CXXFLAGS',
  'CPPFLAGS',
  'LDFLAGS',
  'RUSTC',
  'RUSTDOC',
  'RUSTC_WRAPPER',
  'RUSTC_WORKSPACE_WRAPPER',
  'RUSTC_LINKER',
  'LIBCLANG_PATH',
  'CLANG_PATH',
  'LLVM_CONFIG_PATH',
  'LIBRARY_PATH',
  'LD_LIBRARY_PATH',
  'CPATH',
  'C_INCLUDE_PATH',
  'CPLUS_INCLUDE_PATH',
  'CMAKE_TOOLCHAIN_FILE',
  'CMAKE_PREFIX_PATH',
  'RUSTFLAGS',
  'RUSTDOCFLAGS',
  'CARGO_ENCODED_RUSTFLAGS',
  'CARGO_ENCODED_RUSTDOCFLAGS',
  'RUSTUP_TOOLCHAIN',
])

function contains(directory, location) {
  const prefix = directory.endsWith(path.sep) ? directory : `${directory}${path.sep}`
  return location === directory || location.startsWith(prefix)
}

function canonicalBuildPath(location, workspace, filesystem) {
  const suffix = []
  let directory = path.resolve(workspace, location)
  for (;;) {
    try {
      return path.join(filesystem.realpathSync(directory), ...suffix)
    }
    catch (error) {
      const parent = path.dirname(directory)
      if (error.code !== 'ENOENT' || parent === directory)
        throw error
      suffix.unshift(path.basename(directory))
      directory = parent
    }
  }
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

  // These two jobs use the stock native toolchain. Interpreting arbitrary flag strings,
  // wrappers and symlink aliases would make an "unused SDK" decision unsafe.
  const nativeOverride = Object.keys(environment).find(name => environment[name] && (
    nativeInputs.has(name)
    || /^(?:HOST|TARGET)_/.test(name)
    || /^(?:CC|CXX|AR|ARFLAGS|LD|RANLIB|CFLAGS|CXXFLAGS|CPPFLAGS)_/.test(name)
    || /^PKG_CONFIG(?:_|$)/.test(name)
    || /^BINDGEN_EXTRA_CLANG_ARGS(?:_|$)/.test(name)
    || /^(?:[A-Z0-9_]+_)?OPENSSL_/.test(name)
    || /^CARGO_TARGET_.+_(?:LINKER|AR|RUSTFLAGS|RUNNER)$/.test(name)
    || /^CARGO_BUILD_(?:RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTFLAGS|RUSTDOCFLAGS|TARGET)$/.test(name)
  ))
  if (nativeOverride)
    throw new Error(`Rust SDK reclamation requires stock native tools; explicit ${nativeOverride} overrides are unsupported.`)

  const locations = [environment.GITHUB_WORKSPACE, environment.RUNNER_TEMP, environment.HOME, environment.CARGO_HOME || (environment.HOME && path.join(environment.HOME, '.cargo'))]
  if (locations.some(location => !location || !path.isAbsolute(location)) || environment.HOME !== user.homedir)
    throw new Error('Rust disk preparation requires the runner account and absolute build locations.')

  const canonical = location => filesystem.realpathSync(location)
  const protectedDirectories = locations.map(canonical)
  const workspace = protectedDirectories[0]
  if (canonical((dependencies.cwd ?? process.cwd)()) !== workspace)
    throw new Error('Rust disk preparation must run from the checked-out workspace root.')
  const device = filesystem.statSync(workspace).dev
  if (protectedDirectories.some(location => filesystem.statSync(location).dev !== device))
    throw new Error('Rust build, temporary and Cargo directories must share the measured filesystem.')
  if (['CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR', 'CARGO_BUILD_BUILD_DIR'].some(name => environment[name]))
    throw new Error('Rust disk preparation does not support custom Cargo build directories.')

  for (const directory of UNUSED_SDK_DIRECTORIES) {
    if (protectedDirectories.some(location => contains(directory, location) || contains(location, directory)))
      throw new Error('An unused SDK directory overlaps a protected build location.')
  }

  // SDK inventory variables and unused PATH entries advertise preinstalled tools;
  // other filesystem-valued inputs may select native dependencies through aliases.
  const inaccessiblePathInputs = new Set()
  for (const [name, value] of Object.entries(environment)) {
    if (!value || name === 'PATH' || /^(?:ANDROID_|DOTNET_|SWIFT_)/.test(name))
      continue
    if (!path.isAbsolute(value) && !/(?:DIR|ROOT|PREFIX|LIBDIR|INCLUDEDIR|PATH|HOME|FILE)$/.test(name))
      continue
    for (const candidate of new Set([value, ...value.split(path.delimiter)])) {
      if (!candidate)
        continue
      const declared = path.resolve(workspace, candidate)
      if (UNUSED_SDK_DIRECTORIES.some(directory => contains(directory, declared)))
        throw new Error(`A native filesystem input references an SDK scheduled for reclamation: ${name}`)
      let location
      try {
        location = canonicalBuildPath(candidate, workspace, filesystem)
      }
      catch (error) {
        // A stock search path can contain root-only entries. The unprivileged
        // build cannot use those entries; do not mistake them for active SDKs.
        // Explicit native overrides were already refused above.
        if (error.code === 'EACCES' || error.code === 'EPERM') {
          inaccessiblePathInputs.add(name)
          continue
        }

        throw new Error(`Could not validate the filesystem input: ${name}`)
      }

      if (UNUSED_SDK_DIRECTORIES.some(directory => contains(directory, location)))
        throw new Error(`A native filesystem input references an SDK scheduled for reclamation: ${name}`)
    }
  }

  const cargoConfigs = new Set(['config', 'config.toml'].map(name => path.join(protectedDirectories[3], name)))
  for (let directory = workspace; ; directory = path.dirname(directory)) {
    for (const name of ['config', 'config.toml'])
      cargoConfigs.add(path.join(directory, '.cargo', name))
    if (directory === path.dirname(directory))
      break
  }

  for (const configuration of cargoConfigs) {
    try {
      filesystem.lstatSync(configuration)
    }
    catch (error) {
      if (error.code === 'ENOENT')
        continue
      throw error
    }

    throw new Error(`Rust SDK reclamation does not support Cargo configuration files: ${configuration}`)
  }

  const mounts = filesystem.readFileSync('/proc/self/mountinfo', 'utf8').trim().split('\n').map((line) => {
    const fields = line.split(' ')
    const separator = fields.indexOf('-')
    if (fields.length < 10 || separator < 6 || !fields[4]?.startsWith('/'))
      throw new Error('Could not establish the runner mount topology before reclamation.')
    const decode = value => value.replace(/\\([0-7]{3})/g, (_match, octal) => String.fromCharCode(Number.parseInt(octal, 8)))
    return {
      device: fields[2],
      root: decode(fields[3]),
      point: decode(fields[4]),
      filesystem: fields[separator + 1],
    }
  })
  const roots = mounts.filter(mount => mount.point === '/')
  if (roots.length !== 1 || roots[0].root !== '/' || roots[0].filesystem === 'overlay')
    throw new Error('Could not establish the runner root mount before reclamation.')
  for (const directory of UNUSED_SDK_DIRECTORIES) {
    const aliased = mounts.some(mount => mount.point !== '/' && (
      contains(directory, mount.point) || contains(mount.point, directory)
      || (mount.device === roots[0].device && (contains(directory, mount.root) || contains(mount.root, directory)))
    ))
    if (aliased)
      throw new Error(`Refusing a mounted or aliased SDK directory: ${directory}`)
  }

  const toolLocations = execute('which', buildTools, { encoding: 'utf8', timeout: Math.min(10000, remaining()), stdio: ['ignore', 'pipe', 'pipe'] })
    .trim()
    .split('\n')
    .map(canonical)
  if (toolLocations.length !== buildTools.length)
    throw new Error('Could not identify the active native build tools before SDK reclamation.')
  for (const tool of ['cargo', 'rustc']) {
    const location = execute('rustup', ['which', tool], { encoding: 'utf8', timeout: Math.min(10000, remaining()), stdio: ['ignore', 'pipe', 'pipe'] }).trim()
    toolLocations.push(canonical(location))
  }

  for (const directory of UNUSED_SDK_DIRECTORIES) {
    if (toolLocations.some(location => contains(directory, location)))
      throw new Error(`An active native build tool uses the SDK directory: ${directory}`)
  }

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

    directories.push({ path: directory, status: 'eligible' })
  }

  for (const directory of directories) {
    if (directory.status !== 'eligible')
      continue
    execute('sudo', ['-n', 'rm', '--recursive', '--force', '--one-file-system', '--', directory.path], { stdio: 'inherit', timeout: remaining() })
    directory.status = 'removed'
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
    inaccessiblePathInputs: [...inaccessiblePathInputs],
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
