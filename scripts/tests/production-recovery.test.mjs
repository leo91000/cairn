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
  prepareRecovery,
  verifyInstallation,
  verifyInstallationCompatibility,
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
      verifyCompatibility: async () => {},
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
      verifyCompatibility: async () => { throw new Error('prepare a matching stopped installation backup') },
    })).rejects.toThrow('matching stopped installation backup')
  })

  it('checks reviewed data compatibility, including legacy bootstrap and incompatible or unknown baselines', async () => {
    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-recovery-git-'))
    directories.push(directory)
    const git = (...args) => execFileSync('git', args, { cwd: directory, encoding: 'utf8' }).trim()
    const commitAll = () => {
      git('add', '.')
      git('-c', 'user.name=Recovery Test', '-c', 'user.email=recovery@example.invalid', 'commit', '--quiet', '-m', 'fixture')
      return git('rev-parse', 'HEAD')
    }

    git('init', '--quiet')
    await writeFile(path.join(directory, 'vault.rs'), 'legacy format\n')
    const legacy = commitAll()
    const legacyTree = git('rev-parse', `${legacy}^{tree}`)
    const file = path.join(directory, 'deploy/installations/data-compatibility.json')
    await mkdir(path.dirname(file), { recursive: true })
    const writeContract = async value => writeFile(file, JSON.stringify(value))
    await writeContract({ schema: 1, epoch: 1, legacyTree })
    const adoption = commitAll()
    await expect(verifyInstallationCompatibility(legacy, adoption, { cwd: directory })).resolves.toBeUndefined()
    await expect(verifyInstallationCompatibility(adoption, legacy, { cwd: directory })).rejects.toThrow('Missing')

    // Ordinary compatible source changes keep the reviewed epoch and deploy automatically.
    await writeFile(path.join(directory, 'vault.rs'), 'compatible implementation change\n')
    const compatible = commitAll()
    await expect(verifyInstallationCompatibility(adoption, compatible, { cwd: directory })).resolves.toBeUndefined()
    await expect(verifyInstallationCompatibility(compatible, adoption, { cwd: directory })).resolves.toBeUndefined()

    // A break declared in any module's persisted format blocks both upgrade and rollback.
    await writeContract({ schema: 1, epoch: 2, legacyTree })
    const incompatible = commitAll()
    await expect(verifyInstallationCompatibility(compatible, incompatible, { cwd: directory })).rejects.toThrow('matching stopped installation backup')
    await expect(verifyInstallationCompatibility(incompatible, compatible, { cwd: directory })).rejects.toThrow('matching stopped installation backup')
    await expect(verifyInstallationCompatibility(legacy, incompatible, { cwd: directory })).rejects.toThrow('Unknown')

    await writeContract({ schema: 1, epoch: 1, legacyTree: '0'.repeat(40) })
    const unknown = commitAll()
    await expect(verifyInstallationCompatibility(legacy, unknown, { cwd: directory })).rejects.toThrow('Unknown')
    await writeContract({ schema: 1, epoch: 0, legacyTree })
    const invalid = commitAll()
    await expect(verifyInstallationCompatibility(adoption, invalid, { cwd: directory })).rejects.toThrow('Invalid')
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
