import { execFile } from 'node:child_process'
import {
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const digest = /^sha256:[a-f0-9]{64}$/
const commit = /^[a-f0-9]{40}$/

function compatibilityContract(value) {
  const positive = n => Number.isSafeInteger(n) && n > 0
  const invalid = () => {
    throw new Error('Invalid installation data compatibility contract')
  }

  if (!value || ![1, 2].includes(value.schema) || !positive(value.epoch)
    || (value.legacyTree !== undefined && !commit.test(value.legacyTree))) {
    invalid()
  }

  if (value.schema === 1)
    return value
  if (!positive(value.databaseVersion) || value.databaseVersion > 2147483647)
    invalid()
  const migration = value.automaticMigration
  if (migration === undefined)
    return value
  if (!migration || typeof migration !== 'object')
    invalid()

  const validSources = Array.isArray(migration.fromEpochs) && migration.fromEpochs.length > 0
    && migration.fromEpochs.every(epoch => positive(epoch) && epoch < value.epoch)
    && new Set(migration.fromEpochs).size === migration.fromEpochs.length
  const advancesDatabase = positive(migration.previousDatabaseVersion)
    && migration.previousDatabaseVersion < value.databaseVersion
  const validPlan = typeof migration.plan === 'string' && /^docs\/[\w./-]+\.md$/i.test(migration.plan)
    && migration.plan.split('/').every(part => part !== '.' && part !== '..')
  const validTests = Array.isArray(migration.tests) && migration.tests.length > 0 && migration.tests.length <= 64
    && migration.tests.every(name => typeof name === 'string' && /^[a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)*$/.test(name))
    && new Set(migration.tests).size === migration.tests.length
  if (!validSources || !advancesDatabase || !validPlan || !validTests)
    invalid()

  return value
}

// The complete backend suite ran against this exact source tree. Require each
// declared migration regression to have actually passed, rather than accepting
// an ignored test, a renamed filter matching zero tests, or a declaration alone.
export async function qualifyInstallationMigrations(log, { cwd = process.cwd() } = {}) {
  const contract = compatibilityContract(JSON.parse(await readFile(path.join(cwd, 'deploy/installations/data-compatibility.json'), 'utf8')))
  const migration = contract.automaticMigration
  if (!migration)
    return
  if (!(await readFile(path.join(cwd, migration.plan), 'utf8')).trim())
    throw new Error('The automatic migration requires a forward recovery plan')
  const passed = new Set([...log.matchAll(/^test ([a-z0-9_:]+) \.\.\. ok\r?$/gm)].map(match => match[1]))
  if (migration.tests.some(name => !passed.has(name)))
    throw new Error('The automatic migration requires passing backend regression evidence for every declared test')
}

export async function verifyInstallation(config, {
  ssh,
  sleep = setTimeout,
  now = Date.now,
  timeoutMs = 15 * 60 * 1000,
} = {}) {
  const expected = config.installationImage.split('@')[1]
  if (!digest.test(expected))
    throw new Error('Expected an immutable installation image')
  const deadline = now() + timeoutMs
  while (now() < deadline) {
    const result = JSON.parse((await ssh(`verify ${expected}`)).toString())
    if (result.ready === true && result.manager === true && result.runner === true && result.health === true)
      return result
    await sleep(10000)
  }

  throw new Error('The installation did not become healthy on the approved image; inspect its updater before intervening')
}

export async function verifyInstallationCompatibility(before, after, { cwd } = {}) {
  try {
    await exec('git', ['cat-file', '-e', `${before}^{commit}`], { cwd })
  }
  catch {
    await exec('git', ['fetch', '--no-tags', 'origin', before], { cwd })
  }

  const file = 'deploy/installations/data-compatibility.json'
  const read = async (ref) => {
    const { stdout: entry } = await exec('git', ['ls-tree', ref, '--', file], { cwd })
    if (!entry.trim())
      return null
    const { stdout } = await exec('git', ['show', `${ref}:${file}`], { cwd })
    return compatibilityContract(JSON.parse(stdout))
  }

  const candidate = await read(after)
  if (!candidate)
    throw new Error('Missing installation data compatibility contract')
  const previous = await read(before)
  if (previous) {
    if (candidate.epoch < previous.epoch)
      throw new Error('Installation data downgrade refused: use a forward fix compatible with the current data')
    if (candidate.epoch === previous.epoch) {
      if (candidate.schema !== previous.schema || candidate.databaseVersion !== previous.databaseVersion)
        throw new Error('Database compatibility changed without advancing the installation epoch')
      return
    }

    const migration = candidate.automaticMigration
    if (candidate.schema === 2 && migration?.fromEpochs.includes(previous.epoch)
      && (previous.schema === 1 || previous.databaseVersion === migration.previousDatabaseVersion)) {
      const { stdout: plan } = await exec('git', ['show', `${after}:${migration.plan}`], { cwd })
      if (!plan.trim())
        throw new Error('The automatic migration requires a forward recovery plan')
      return
    }

    throw new Error('Installation data compatibility changed: use a reviewed forward migration procedure before deployment')
  }

  // The first adoption accepts only the reviewed pre-contract release tree.
  const { stdout: tree } = await exec('git', ['rev-parse', `${before}^{tree}`], { cwd })
  if (candidate.epoch !== 1 || candidate.legacyTree !== tree.trim())
    throw new Error('Unknown installation data baseline: use a reviewed forward migration procedure before deployment')
}

export async function checkInstallationCompatibility(config, { fetchHealth = fetch, verifyCompatibility = verifyInstallationCompatibility } = {}) {
  const response = await fetchHealth(new URL('/health', config.publicUrl), { signal: AbortSignal.timeout(20000) })
  if (!response.ok)
    throw new Error('Current production health is unavailable')
  const health = await response.json()
  if (health.status !== 'ok' || !commit.test(health.commit ?? ''))
    throw new Error('Current production health identity is unavailable')
  await verifyCompatibility(health.commit, config.commit)
}

async function withSsh(action) {
  const host = process.env.CAIRN_RELEASE_SSH_HOST
  const user = process.env.CAIRN_RELEASE_SSH_USER
  const key = process.env.CAIRN_RELEASE_SSH_KEY
  const knownHosts = process.env.CAIRN_RELEASE_SSH_KNOWN_HOSTS
  if (!/^[a-z0-9.-]+$/i.test(host ?? '') || !/^[a-z_][a-z0-9_-]*$/.test(user ?? '') || !key || !knownHosts)
    throw new Error('Configure the restricted release SSH identity and pinned host key')

  const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-release-ssh-'))
  try {
    const identity = path.join(directory, 'identity')
    const hosts = path.join(directory, 'known_hosts')
    await writeFile(identity, `${key.trim()}\n`, { mode: 0o600 })
    await writeFile(hosts, `${knownHosts.trim()}\n`, { mode: 0o600 })
    const env = { ...process.env }
    delete env.CAIRN_RELEASE_SSH_KEY
    const args = ['-i', identity, '-o', 'BatchMode=yes', '-o', 'IdentitiesOnly=yes', '-o', 'IdentityAgent=none', '-o', 'StrictHostKeyChecking=yes', '-o', `UserKnownHostsFile=${hosts}`, '-o', 'ConnectTimeout=15', `${user}@${host}`]
    const ssh = async (command) => {
      try {
        const { stdout } = await exec('ssh', [...args, command], {
          env,
          encoding: 'buffer',
          timeout: 16 * 60 * 1000,
          maxBuffer: 64 * 1024,
        })
        return stdout
      }
      catch {
        // Child-process errors include stderr and environment: never print them.
        throw new Error('Restricted production release SSH request failed')
      }
    }

    return await action(ssh)
  }
  finally {
    await rm(directory, { recursive: true, force: true })
  }
}

if (import.meta.main) {
  const config = {
    tag: process.env.GITHUB_REF_NAME,
    commit: process.env.GITHUB_SHA,
    installationImage: process.env.DEPLOY_INSTALLATION_IMAGE,
    publicUrl: process.env.CAIRN_BEACON_ORIGIN,
  }
  try {
    if (process.argv[2] === 'qualify') {
      const log = await readFile(process.argv[3], 'utf8')
      await qualifyInstallationMigrations(log)
      console.log('Declared automatic migrations passed their backend regressions.')
      process.exit(0)
    }

    if (!/^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(config.tag ?? '') || !commit.test(config.commit ?? ''))
      throw new Error('Production release requires an exact stable release tag and commit')
    if (process.argv[2] === 'verify') {
      await withSsh(ssh => verifyInstallation(config, { ssh }))
      console.log('Installation manager and runner are healthy on the approved image.')
    }
    else if (process.argv[2] === 'check') {
      await checkInstallationCompatibility(config)
      console.log('Installation data compatibility verified.')
    }
    else {
      throw new Error('Expected check or verify')
    }
  }
  catch (error) {
    // Only explicitly created validation errors are safe to expose. Git/filesystem
    // errors can contain request details, so keep their diagnostics on the host.
    console.error(error instanceof Error && !error.code ? error.message : 'Production release verification failed')
    process.exitCode = 1
  }
}
