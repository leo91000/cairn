import { execFileSync } from 'node:child_process'
import fs from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

const phases = new Set(['before-restore', 'after-restore', 'before-save'])
const errorCodes = new Set(['ENOENT', 'EACCES', 'EPERM', 'ETIMEDOUT', 'ENOSPC', 'EIO'])

function errorCode(error) {
  if (errorCodes.has(error?.code))
    return error.code
  return 'unavailable'
}

function filesystem(location, statfs) {
  try {
    const value = statfs(location, { bigint: true })
    return {
      readable: true,
      blockBytes: `${value.bsize}`,
      availableBytes: `${value.bavail * value.bsize}`,
      freeBytes: `${value.bfree * value.bsize}`,
      totalBytes: `${value.blocks * value.bsize}`,
      totalInodes: `${value.files}`,
      freeInodes: `${value.ffree}`,
    }
  }
  catch (error) {
    return { readable: false, errorCode: errorCode(error) }
  }
}

function usage(location, { execute, now, deadline }) {
  const result = {}
  const measurements = [
    ['allocatedBytes', ['-B1']],
    ['apparentBytes', ['-B1', '--apparent-size']],
    ['inodes', ['--inodes']],
  ]
  for (const [name, options] of measurements) {
    const remaining = deadline - now()
    if (remaining <= 0) {
      result[name] = { errorCode: 'observation_budget_exhausted' }
      continue
    }

    try {
      const output = execute('du', ['-x', '--summarize', ...options, '--', location], {
        encoding: 'utf8',
        timeout: remaining,
        maxBuffer: 1024,
        stdio: ['ignore', 'pipe', 'pipe'],
      })
      const match = /^(\d+)\s/.exec(output)
      result[name] = match ? { value: match[1] } : { errorCode: 'invalid_measurement' }
    }
    catch (error) {
      result[name] = { errorCode: errorCode(error) }
    }
  }

  return result
}

function publicMetadata(environment) {
  const metadata = {}
  for (const [name, key, pattern] of [
    ['head', 'GITHUB_SHA', /^[0-9a-f]{40}$/],
    ['job', 'GITHUB_JOB', /^[\w-]{1,100}$/],
    ['run', 'GITHUB_RUN_ID', /^\d{1,20}$/],
    ['attempt', 'GITHUB_RUN_ATTEMPT', /^\d{1,10}$/],
    ['imageOS', 'ImageOS', /^[\w.-]{1,80}$/],
    ['imageVersion', 'ImageVersion', /^[\w.-]{1,80}$/],
  ]) {
    const value = environment[key]
    if (typeof value === 'string' && pattern.test(value))
      metadata[name] = value
  }

  return metadata
}

export function observeRustCache(phase, environment, dependencies = {}) {
  if (!phases.has(phase))
    throw new Error('Unknown cache observation phase.')
  if (environment.GITHUB_ACTIONS !== 'true')
    throw new Error('Cache observations run only on a coordinated disposable CI host.')
  for (const name of ['GITHUB_WORKSPACE', 'RUNNER_TEMP', 'HOME']) {
    if (typeof environment[name] !== 'string' || !path.isAbsolute(environment[name]))
      throw new Error('Cache observation requires absolute CI filesystem locations.')
  }

  const workspace = environment.GITHUB_WORKSPACE
  const temporary = environment.RUNNER_TEMP
  const cargo = environment.CARGO_HOME || path.join(environment.HOME, '.cargo')
  if (!path.isAbsolute(cargo))
    throw new Error('Cache observation requires an absolute Cargo location.')
  const statfs = dependencies.statfs ?? fs.statfsSync
  const execute = dependencies.execute ?? execFileSync
  const now = dependencies.now ?? Date.now
  const startedAt = now()
  // The whole observation has one budget; a large directory cannot multiply it.
  const deadline = startedAt + 15000
  const capacity = {
    workspace: filesystem(workspace, statfs),
    temporary: filesystem(temporary, statfs),
    cargo: filesystem(cargo, statfs),
  }
  const locations = {
    targetDebug: path.join(workspace, 'target', 'debug'),
    excludedIncremental: path.join(workspace, 'target', 'debug', 'incremental'),
    registryIndex: path.join(cargo, 'registry', 'index'),
    registryArchives: path.join(cargo, 'registry', 'cache'),
    cargoGit: path.join(cargo, 'git'),
  }
  const measurements = {}
  for (const [label, location] of Object.entries(locations))
    measurements[label] = usage(location, { execute, now, deadline })
  return {
    schemaVersion: 1,
    at: new Date(startedAt).toISOString(),
    phase,
    metadata: publicMetadata(environment),
    elapsedMs: now() - startedAt,
    filesystem: capacity,
    usage: measurements,
    scope: 'Directory aggregates, not archive expanded-size or peak-restore measurement. Incremental is excluded by cache policy; reported separately.',
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [, , phase, destination] = process.argv
  if (!destination || !phases.has(phase))
    throw new Error('Usage: ci-rust-cache-observation.mjs PHASE OUTPUT.json')
  const observation = observeRustCache(phase, process.env)
  fs.mkdirSync(path.dirname(destination), { recursive: true })
  fs.writeFileSync(destination, `${JSON.stringify(observation)}\n`, { mode: 0o600 })
  console.log(JSON.stringify(observation))
}
