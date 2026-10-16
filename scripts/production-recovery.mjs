import { execFile } from 'node:child_process'
import { createHash, randomBytes } from 'node:crypto'
import {
  appendFile,
  mkdir,
  mkdtemp,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const maximumBackup = 512 * 1024 * 1024
const digest = /^sha256:[a-f0-9]{64}$/
const commit = /^[a-f0-9]{40}$/

export function verifyProof(proof, tag, identifier, now = Date.now()) {
  if (proof.schema !== 1 || proof.id !== identifier || proof.release !== tag
    || proof.restore?.verified !== true || !Number.isSafeInteger(proof.restore.sourceTables)
    || proof.restore.sourceTables <= 0 || proof.restore.sourceTables !== proof.restore.restoredTables
    || !Number.isSafeInteger(proof.encryptedSize) || proof.encryptedSize <= 0 || proof.encryptedSize > maximumBackup
    || !/^[a-f0-9]{64}$/.test(proof.encryptedSha256)
    || !Number.isSafeInteger(proof.createdAt) || now - proof.createdAt > 15 * 60 * 1000 || proof.createdAt > now + 60 * 1000
    || !commit.test(proof.previous?.commit)
    || !/^ghcr\.io\/[a-z0-9_.\-/]+@sha256:[a-f0-9]{64}$/.test(proof.previous?.beaconImage)
    || !/^ghcr\.io\/[a-z0-9_.\-/]+@sha256:[a-f0-9]{64}$/.test(proof.previous?.installationImage)) {
    throw new Error('Production recovery evidence is missing, stale, or inconsistent')
  }

  return proof
}

export async function prepareRecovery(config, {
  ssh,
  changedInstallationFiles,
  now = Date.now,
  identifier = randomBytes(16).toString('hex'),
} = {}) {
  const raw = await ssh(`prepare ${config.tag} ${identifier}`)
  const proof = verifyProof(JSON.parse(raw.toString()), config.tag, identifier, now())
  const ciphertext = await ssh(`download ${identifier}`)
  if (ciphertext.length !== proof.encryptedSize || createHash('sha256').update(ciphertext).digest('hex') !== proof.encryptedSha256)
    throw new Error('The downloaded encrypted backup does not match its recovery evidence')

  await mkdir(config.directory, { recursive: true, mode: 0o700 })
  await writeFile(path.join(config.directory, 'beacon.dump.age'), ciphertext, { mode: 0o600, flag: 'wx' })
  const record = { ...proof, candidate: { commit: config.commit, beaconImage: config.beaconImage, installationImage: config.installationImage } }
  await writeFile(path.join(config.directory, 'release.json'), `${JSON.stringify(record, null, 2)}\n`, { mode: 0o600, flag: 'wx' })

  // Changes to the installation database or disk format need a matching stopped
  // installation backup; a Beacon Postgres dump cannot roll them back.
  const changed = await changedInstallationFiles(proof.previous.commit, config.commit)
  if (changed.length)
    throw new Error('Installation schema/storage code changed: prepare a matching stopped installation backup and a reviewed rollout before deployment')

  return record
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

export async function installationRecoveryChanges(before, after, { cwd } = {}) {
  try {
    await exec('git', ['cat-file', '-e', `${before}^{commit}`], { cwd })
  }
  catch {
    await exec('git', ['fetch', '--no-tags', 'origin', before], { cwd })
  }

  const paths = [
    'crates/installation/src/store.rs',
    'crates/installation/src/storage/',
    'crates/installation/src/nodes/',
    'crates/installation/src/object_storage.rs',
    'crates/installation/src/models.rs',
    'crates/installation/src/accounts/',
    'crates/installation/src/service.rs',
  ]
  const { stdout } = await exec('git', ['diff', '--name-only', before, after, '--', ...paths], { cwd })
  return stdout.trim().split('\n').filter(Boolean)
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
          maxBuffer: maximumBackup,
        })
        return stdout
      }
      catch {
        // Child-process errors include stderr and environment: never print them.
        throw new Error('Restricted production recovery SSH request failed')
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
    beaconImage: process.env.DEPLOY_IMAGE,
    installationImage: process.env.DEPLOY_INSTALLATION_IMAGE,
    directory: process.env.PRODUCTION_RECOVERY_DIRECTORY,
  }
  try {
    if (!/^v(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/.test(config.tag ?? '') || !commit.test(config.commit ?? ''))
      throw new Error('Production recovery requires an exact stable release tag and commit')
    if (process.argv[2] === 'verify') {
      await withSsh(ssh => verifyInstallation(config, { ssh }))
      console.log('Installation manager and runner are healthy on the approved image.')
    }
    else if (process.argv[2] === 'prepare') {
      const record = await withSsh(ssh => prepareRecovery(config, { ssh, changedInstallationFiles: installationRecoveryChanges }))
      if (process.env.GITHUB_OUTPUT)
        await appendFile(process.env.GITHUB_OUTPUT, `backup-id=${record.id}\n`)
      console.log('Postgres backup restored successfully; encrypted recovery artifact downloaded for off-host retention.')
    }
    else {
      throw new Error('Expected prepare or verify')
    }
  }
  catch (error) {
    // Only explicitly created validation errors are safe to expose. Git/filesystem
    // errors can contain request details, so keep their diagnostics on the host.
    console.error(error instanceof Error && !error.code ? error.message : 'Production recovery preparation failed')
    process.exitCode = 1
  }
}
