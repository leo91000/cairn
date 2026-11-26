import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  closeSync,
  constants,
  fstatSync,
  lstatSync,
  openSync,
  readlinkSync,
  readSync,
  realpathSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath, pathToFileURL } from 'node:url'

const metadata = ['package.json', 'pnpm-lock.yaml', 'pnpm-workspace.yaml']
// The provider, official Rust SDK client, stdio fixture and their tsx launcher.
const requiredImports = ['tsx', '@fastify/formbody', '@modelcontextprotocol/client', '@modelcontextprotocol/node', '@modelcontextprotocol/server', '@modelcontextprotocol/server/stdio', 'fastify', 'zod']
const digest = bytes => createHash('sha256').update(bytes).digest('hex')
const gitHead = cwd => execFileSync('git', ['rev-parse', 'HEAD'], { cwd, encoding: 'utf8', timeout: 10000 }).trim()
const gitFile = (cwd, sha, file) => execFileSync('git', ['show', `${sha}:${file}`], { cwd, timeout: 10000 })

function regular(file) {
  const fd = openSync(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const stat = fstatSync(fd)
    if (!stat.isFile() || stat.size > 1024 * 1024)
      throw new Error('Invalid bounded dependency provenance file')
    const bytes = Buffer.alloc(1024 * 1024 + 1)
    const size = readSync(fd, bytes, 0, bytes.length, 0)
    if (size > 1024 * 1024)
      throw new Error('Dependency provenance file exceeds its bound')
    return bytes.subarray(0, size)
  }
  finally { closeSync(fd) }
}

function physicalDirectory(directory) {
  if (!path.isAbsolute(directory) || !lstatSync(directory).isDirectory()
    || lstatSync(directory).isSymbolicLink() || realpathSync(directory) !== path.resolve(directory)) {
    throw new Error('Dependency context requires physical directories')
  }
}

function applicationImporter(lock) {
  // Accept only the pinned pnpm12 root-importer scalar layout. Unsupported lock
  // syntax must refuse admission rather than guess a package's store identity.
  const importer = lock.match(/^importers:\r?\n\r?\n {2}\.:\r?\n([\s\S]*?)(?=^\S)/m)?.[1]
  if (!importer)
    throw new Error('Unsupported installed application importer')
  const packages = {}
  let group
  let name
  for (const line of importer.split(/\r?\n/)) {
    if (!line.trim())
      continue
    const section = line.match(/^ {4}(dependencies|devDependencies|optionalDependencies):$/)
    if (section) {
      group = section[1]
      name = undefined
      continue
    }

    const dependency = line.match(/^ {6}(?:'([^']+)'|([\w@/.-]+)):$/)
    if (group && dependency) {
      name = dependency[1] ?? dependency[2]
      if (Object.hasOwn(packages, name))
        throw new Error('Ambiguous installed application dependency')
      packages[name] = { group }
      continue
    }

    const value = line.match(/^ {8}(specifier|version): ([^\s'"\\]+)$/)
    if (!name || !value || Object.hasOwn(packages[name], value[1]))
      throw new Error('Unsupported installed application importer syntax')
    packages[name][value[1]] = value[2]
  }

  return packages
}

function installedPackagePath(virtualStore, name, locked, expected, maxLength) {
  if (!locked || locked.specifier !== expected || !/^\d+\.\d+\.\d+(?:-[\w.-]+)?$/.test(expected)
    || !new RegExp(`^${expected.replaceAll('.', '\\.')}($|\\()`).test(locked.version ?? '')
    || !/^[\w@/().+-]+$/.test(locked.version)) {
    throw new Error('Required dependency differs from the pinned application importer')
  }

  // pnpm's supported unshortened depPathToFilename form (including peers).
  // Refuse identities requiring truncation/hashing rather than infer that path.
  const identity = `${name}@${locked.version}`.replaceAll('/', '+').replaceAll(')(', '_').replaceAll('(', '_').replaceAll(')', '')
  if (!Number.isInteger(maxLength) || maxLength < identity.length || maxLength > 120)
    throw new Error('Unsupported required dependency virtual-store identity')
  return path.join(virtualStore, identity, 'node_modules', name)
}

function resolveEntrypoints(observer) {
  // Resolve using Node's ESM import conditions at the actual fixture ancestry.
  // Resolution does not execute package code, and Node can return a missing
  // exports target: each returned file is separately checked below.
  const probe = `const imports = JSON.parse(process.argv[1]); const parent = process.argv[2]; console.log(JSON.stringify(Object.fromEntries(imports.map(name => [name, import.meta.resolve(name, parent)]))))`
  return JSON.parse(execFileSync(process.execPath, [
    '--experimental-import-meta-resolve',
    '--input-type=module',
    '-e',
    probe,
    JSON.stringify(requiredImports),
    pathToFileURL(path.join(observer, 'tests/fixtures/mcp.mjs')).href,
  ], {
    cwd: observer,
    encoding: 'utf8',
    timeout: 10000,
    env: { PATH: process.env.PATH },
    stdio: ['ignore', 'pipe', 'pipe'],
  }))
}

function inspect({
  observer,
  candidate,
  runnerTemp,
  source,
  observerSha,
  readHead = gitHead,
  readTracked = gitFile,
}) {
  for (const directory of [observer, candidate, runnerTemp])
    physicalDirectory(directory)
  if (candidate !== path.join(runnerTemp, 'cairn-diagnostic-candidate') || observer === candidate
    || !/^[a-f0-9]{40}$/.test(observerSha ?? '') || !/^[a-f0-9]{40}$/.test(source.revisions?.candidate ?? '')
    || source.observer !== observerSha || source.sources?.candidate !== candidate
    || source.diagnosticOnly !== true || source.qualificationClaim !== false
    || readHead(observer) !== observerSha || readHead(candidate) !== source.revisions.candidate) {
    throw new Error('Dependency context does not match the pinned source')
  }

  const hashes = {}
  let manifest
  let lock
  for (const file of metadata) {
    const observed = regular(path.join(observer, file))
    const pinned = regular(path.join(candidate, file))
    if (!observed.equals(pinned) || !observed.equals(readTracked(observer, observerSha, file))
      || !pinned.equals(readTracked(candidate, source.revisions.candidate, file))) {
      throw new Error('Observer and candidate dependency metadata differ from their pinned Git sources')
    }

    hashes[file] = digest(pinned)
    if (file === 'package.json')
      manifest = JSON.parse(pinned)
    if (file === 'pnpm-lock.yaml')
      lock = pinned.toString()
  }

  const modules = path.join(observer, 'node_modules')
  physicalDirectory(modules)
  const virtualStore = path.join(modules, '.pnpm')
  physicalDirectory(virtualStore)
  const installationBytes = regular(path.join(modules, '.modules.yaml'))
  const installation = JSON.parse(installationBytes)
  if (manifest.packageManager !== 'pnpm@12.3.4' || installation.packageManager !== manifest.packageManager
    || installation.nodeLinker !== 'isolated' || installation.virtualStoreDir !== '.pnpm' || installation.layoutVersion !== 5
    || !['dependencies', 'devDependencies', 'optionalDependencies'].every(key => installation.included?.[key] === true)) {
    throw new Error('Unsupported or incomplete observer installation provenance')
  }

  // pnpm12's first document pins pnpm itself; its final document pins the app.
  // Compare that complete document, never a subset of installed dependencies.
  const documents = lock.split(/^---[ \t]*\r?$/m).map(part => part.trim()).filter(Boolean)
  const installedLock = regular(path.join(virtualStore, 'lock.yaml'))
  if (documents.length !== 2 || documents[1] !== installedLock.toString().trim())
    throw new Error('Installed application lock differs from the exact source lock')
  const importer = applicationImporter(documents[1])
  const entrypoints = resolveEntrypoints(observer)
  const packages = {}
  for (const specifier of requiredImports) {
    const name = specifier.startsWith('@') ? specifier.split('/').slice(0, 2).join('/') : specifier.split('/')[0]
    const location = path.join(modules, name)
    const scope = path.dirname(location)
    if (scope !== modules)
      physicalDirectory(scope)
    const expected = manifest.dependencies?.[name] ?? manifest.devDependencies?.[name]
    const declaredGroup = manifest.dependencies?.[name] ? 'dependencies' : 'devDependencies'
    const resolved = installedPackagePath(virtualStore, name, importer[name], expected, installation.virtualStoreDirMaxLength)
    physicalDirectory(resolved)
    if (importer[name].group !== declaredGroup || realpathSync(location) !== resolved)
      throw new Error('Required dependency does not resolve to its locked virtual-store package')
    const manifestBytes = regular(path.join(resolved, 'package.json'))
    const entry = JSON.parse(manifestBytes)
    if (!expected || entry.name !== name || entry.version !== expected)
      throw new Error('Required dependency version differs from the pinned package manifest')
    const entrypoint = fileURLToPath(entrypoints[specifier])
    if (!entrypoint.startsWith(`${resolved}${path.sep}`) || realpathSync(entrypoint) !== entrypoint)
      throw new Error('Required dependency entrypoint escapes its locked package')
    const entrypointBytes = regular(entrypoint)
    packages[name] ??= {
      version: entry.version,
      lockedVersion: importer[name].version,
      packagePath: resolved,
      manifestPath: path.join(resolved, 'package.json'),
      manifestSHA256: digest(manifestBytes),
      entrypoints: {},
    }
    packages[name].entrypoints[specifier] = { path: entrypoint, sha256: digest(entrypointBytes) }
  }

  return {
    diagnosticOnly: true,
    qualificationClaim: false,
    observer: observerSha,
    candidate: source.revisions.candidate,
    metadata: hashes,
    packageManager: manifest.packageManager,
    installationSHA256: digest(installationBytes),
    installedLockSHA256: digest(installedLock),
    applicationLockSHA256: digest(documents[1]),
    packages,
    bridgeTarget: modules,
  }
}

export function prepareBackendDependencies(options) {
  const proof = inspect(options)
  const destination = path.join(options.candidate, 'node_modules')
  try {
    lstatSync(destination)
  }
  catch (error) {
    if (error.code !== 'ENOENT')
      throw error
    symlinkSync(proof.bridgeTarget, destination, 'dir')
    return proof
  }

  throw new Error('Pinned candidate already has an unowned dependency context')
}

export function verifyBackendDependencies(options, recorded) {
  const proof = inspect(options)
  const destination = path.join(options.candidate, 'node_modules')
  if (!lstatSync(destination).isSymbolicLink() || readlinkSync(destination) !== proof.bridgeTarget
    || realpathSync(destination) !== proof.bridgeTarget || JSON.stringify(recorded) !== JSON.stringify(proof)) {
    throw new Error('Candidate dependency bridge/provenance differs from its acquisition receipt')
  }

  const entrypoints = resolveEntrypoints(options.candidate)
  for (const entry of Object.values(proof.packages)) {
    for (const [specifier, expected] of Object.entries(entry.entrypoints)) {
      if (fileURLToPath(entrypoints[specifier]) !== expected.path)
        throw new Error('Candidate fixture resolves outside its acquired dependency context')
    }
  }

  return proof
}

export function dependencyOptions(env = process.env) {
  const observer = env.GITHUB_WORKSPACE
  const source = JSON.parse(regular(path.join(observer, 'backend-diagnostics/source.json')))
  return {
    observer,
    candidate: env.CI_DIAGNOSTIC_SOURCE,
    runnerTemp: env.RUNNER_TEMP,
    source,
    observerSha: env.GITHUB_SHA,
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const options = dependencyOptions()
  const proof = prepareBackendDependencies(options)
  writeFileSync(path.join(options.observer, 'backend-diagnostics/dependencies.json'), `${JSON.stringify(proof)}\n`, { mode: 0o600, flag: 'wx' })
}
