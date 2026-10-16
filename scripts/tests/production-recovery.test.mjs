import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { readFileSync } from 'node:fs'
import {
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import {
  afterEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { parse } from 'yaml'
import {
  installationRecoveryChanges,
  prepareRecovery,
  verifyInstallation,
  verifyProof,
} from '../production-recovery.mjs'

const identifier = 'a'.repeat(32)
const ciphertext = Buffer.from('synthetic age ciphertext')
const image = `ghcr.io/owner/cairn@sha256:${'b'.repeat(64)}`
const now = 1000000

function proof() {
  return {
    schema: 1,
    id: identifier,
    release: 'v0.56.2',
    createdAt: now,
    previous: { commit: 'c'.repeat(40), beaconImage: image, installationImage: image },
    restore: { verified: true, sourceTables: 21, restoredTables: 21 },
    encryptedSize: ciphertext.length,
    encryptedSha256: createHash('sha256').update(ciphertext).digest('hex'),
  }
}

const directories = []

afterEach(async () => {
  for (const directory of directories.splice(0))
    await rm(directory, { recursive: true, force: true })
})

describe('production recovery', () => {
  it('refuses missing, stale, unrelated, oversized, and unsuccessful recovery evidence', () => {
    const invalid = [
      { schema: 2 },
      { id: 'd'.repeat(32) },
      { release: 'v0.56.1' },
      { createdAt: 0 },
      { encryptedSize: 0 },
      { encryptedSize: 513 * 1024 * 1024 },
      { encryptedSha256: 'bad' },
      { restore: { verified: false, sourceTables: 21, restoredTables: 21 } },
      { restore: { verified: true, sourceTables: 21, restoredTables: 20 } },
      { previous: { commit: 'main', beaconImage: image, installationImage: image } },
    ]
    expect(verifyProof(proof(), 'v0.56.2', identifier, now)).toEqual(proof())
    for (const change of invalid)
      expect(() => verifyProof({ ...proof(), ...change }, 'v0.56.2', identifier, now)).toThrow()
  })

  it('downloads only matching ciphertext and retains the previous and candidate pairs privately', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-recovery-test-'))
    directories.push(directory)
    const ssh = vi.fn().mockResolvedValueOnce(Buffer.from(JSON.stringify(proof()))).mockResolvedValueOnce(ciphertext)
    const config = {
      tag: 'v0.56.2',
      commit: 'd'.repeat(40),
      beaconImage: image,
      installationImage: image,
      directory,
    }
    const record = await prepareRecovery(config, {
      ssh,
      changedInstallationFiles: async () => [],
      now: () => now,
      identifier,
    })
    expect(ssh.mock.calls.map(call => call[0])).toEqual([`prepare v0.56.2 ${identifier}`, `download ${identifier}`])
    expect(await readFile(path.join(directory, 'beacon.dump.age'))).toEqual(ciphertext)
    expect((await stat(path.join(directory, 'beacon.dump.age'))).mode & 0o777).toBe(0o600)
    expect(JSON.parse(await readFile(path.join(directory, 'release.json'), 'utf8'))).toEqual(record)
    expect(record.previous).toEqual(proof().previous)
    expect(record.candidate.commit).toBe(config.commit)
  })

  it('rejects a corrupted download before publishing an off-host recovery artifact', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-recovery-test-'))
    directories.push(directory)
    const ssh = vi.fn().mockResolvedValueOnce(Buffer.from(JSON.stringify(proof()))).mockResolvedValueOnce(Buffer.from('corruption'))
    await expect(prepareRecovery({ tag: 'v0.56.2', directory }, { ssh, now: () => now, identifier })).rejects.toThrow('does not match')
    await expect(stat(path.join(directory, 'release.json'))).rejects.toThrow()
  })

  it('stops before deployment when the installation requires its own recovery plan', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-recovery-test-'))
    directories.push(directory)
    const ssh = vi.fn().mockResolvedValueOnce(Buffer.from(JSON.stringify(proof()))).mockResolvedValueOnce(ciphertext)
    await expect(prepareRecovery({ tag: 'v0.56.2', directory }, {
      ssh,
      now: () => now,
      identifier,
      changedInstallationFiles: async () => ['crates/installation/src/store.rs'],
    })).rejects.toThrow('matching stopped installation backup')
  })

  it('detects existing schema and persistent disk format changes against the served commit', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-recovery-git-'))
    directories.push(directory)
    const git = (...args) => execFileSync('git', args, { cwd: directory, encoding: 'utf8' }).trim()
    git('init', '--quiet')
    const schemaFiles = [
      'store.rs',
      'storage/migration.rs',
      'nodes/shared_blocks.rs',
      'nodes/snapshots.rs',
      'nodes/publication.rs',
      'object_storage.rs',
      'models.rs',
      'accounts/mod.rs',
      'service.rs',
    ].map(file => `crates/installation/src/${file}`)
    const unrelated = 'crates/installation/src/chats.rs'
    for (const file of [...schemaFiles, unrelated]) {
      await mkdir(path.dirname(path.join(directory, file)), { recursive: true })
      await writeFile(path.join(directory, file), 'before\n')
    }

    git('add', '.')
    git('-c', 'user.name=Recovery Test', '-c', 'user.email=recovery@example.invalid', 'commit', '--quiet', '-m', 'baseline')
    const before = git('rev-parse', 'HEAD')
    for (const file of [...schemaFiles, unrelated])
      await writeFile(path.join(directory, file), 'after\n')
    git('add', '.')
    git('-c', 'user.name=Recovery Test', '-c', 'user.email=recovery@example.invalid', 'commit', '--quiet', '-m', 'candidate')
    const changed = await installationRecoveryChanges(before, 'HEAD', { cwd: directory })
    expect(changed.sort()).toEqual(schemaFiles.sort())
    expect(changed).not.toContain(unrelated)
  })

  it('waits through an updater replacement and requires the manager, runner, and execution health', async () => {
    const ssh = vi.fn().mockResolvedValueOnce(Buffer.from('{"ready":false}')).mockResolvedValueOnce(Buffer.from('{"ready":true,"manager":true,"runner":true,"health":true}'))
    const sleep = vi.fn()
    expect(await verifyInstallation({ installationImage: image }, { ssh, sleep, now: () => now })).toMatchObject({ ready: true })
    expect(sleep).toHaveBeenCalledOnce()
  })

  it('keeps recovery upload before deployment and installation checks after deployment', () => {
    const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
    const steps = workflow.jobs.deploy.steps
    const prepare = steps.findIndex(step => step.run === 'node scripts/production-recovery.mjs prepare')
    const upload = steps.findIndex(step => step.uses?.startsWith('actions/upload-artifact@'))
    const deploy = steps.findIndex(step => step.run === 'node scripts/deploy-coolify.mjs')
    const verify = steps.findIndex(step => step.run === 'node scripts/production-recovery.mjs verify')
    expect(prepare).toBeGreaterThan(-1)
    expect(upload).toBeGreaterThan(prepare)
    expect(deploy).toBeGreaterThan(upload)
    expect(verify).toBeGreaterThan(deploy)
    expect(steps[deploy].env.COOLIFY_TOKEN).toContain('secrets.CAIRN_RELEASE_COOLIFY_TOKEN')
    expect(steps[upload].with['if-no-files-found']).toBe('error')
    expect(workflow.jobs.deploy.if).toContain('startsWith(github.ref, \'refs/tags/v\')')
    expect(workflow.jobs.deploy.concurrency['cancel-in-progress']).toBe(false)
  })
})
