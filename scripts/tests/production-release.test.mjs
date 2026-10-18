import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import {
  mkdir,
  mkdtemp,
  rm,
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
import { checkInstallationCompatibility, verifyInstallation, verifyInstallationCompatibility } from '../production-release.mjs'

const image = `ghcr.io/owner/cairn@sha256:${'b'.repeat(64)}`
const now = 1000000

const directories = []

afterEach(async () => {
  for (const directory of directories.splice(0))
    await rm(directory, { recursive: true, force: true })
})

describe('production release', () => {
  it('reads the served identity and fails closed on unavailable or invalid health', async () => {
    const verifyCompatibility = vi.fn()
    const config = { publicUrl: 'https://cairn.example', commit: 'd'.repeat(40) }
    const fetchHealth = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ status: 'ok', commit: 'c'.repeat(40) }) })
    await checkInstallationCompatibility(config, { fetchHealth, verifyCompatibility })
    expect(verifyCompatibility).toHaveBeenCalledWith('c'.repeat(40), config.commit)
    for (const response of [{ ok: false }, { ok: true, json: async () => ({ status: 'ok', commit: 'main' }) }]) {
      verifyCompatibility.mockClear()
      fetchHealth.mockResolvedValue(response)
      await expect(checkInstallationCompatibility(config, { fetchHealth, verifyCompatibility })).rejects.toThrow('unavailable')
      expect(verifyCompatibility).not.toHaveBeenCalled()
    }

    fetchHealth.mockResolvedValue({ ok: true, json: async () => ({ status: 'ok', commit: 'c'.repeat(40) }) })
    verifyCompatibility.mockRejectedValue(new Error('Incompatible epoch'))
    await expect(checkInstallationCompatibility(config, { fetchHealth, verifyCompatibility })).rejects.toThrow('Incompatible epoch')
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
    await expect(verifyInstallationCompatibility(compatible, incompatible, { cwd: directory })).rejects.toThrow('reviewed forward migration')
    await expect(verifyInstallationCompatibility(incompatible, compatible, { cwd: directory })).rejects.toThrow('reviewed forward migration')
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

  it('checks compatibility before deployment and installation readiness afterwards without backups', () => {
    const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
    const steps = workflow.jobs.deploy.steps
    const check = steps.findIndex(step => step.run === 'node scripts/production-release.mjs check')
    const upload = steps.findIndex(step => step.uses?.startsWith('actions/upload-artifact@'))
    const deploy = steps.findIndex(step => step.run === 'node scripts/deploy-coolify.mjs')
    const verify = steps.findIndex(step => step.run === 'node scripts/production-release.mjs verify')
    expect(check).toBeGreaterThan(-1)
    expect(upload).toBe(-1)
    expect(deploy).toBeGreaterThan(check)
    expect(verify).toBeGreaterThan(deploy)
    expect(steps[deploy].env.COOLIFY_TOKEN).toContain('secrets.CAIRN_RELEASE_COOLIFY_TOKEN')
    expect(workflow.jobs.deploy.if).toContain('startsWith(github.ref, \'refs/tags/v\')')
    expect(workflow.jobs.deploy.concurrency['cancel-in-progress']).toBe(false)
  })
})
