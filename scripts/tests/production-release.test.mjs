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
import {
  checkInstallationCompatibility,
  qualifyInstallationMigrations,
  verifyInstallation,
  verifyInstallationCompatibility,
} from '../production-release.mjs'

const image = `ghcr.io/owner/cairn@sha256:${'b'.repeat(64)}`
const now = 1000000

const directories = []

async function compatibilityRepository() {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-compatibility-git-'))
  directories.push(directory)
  const git = (...args) => execFileSync('git', args, { cwd: directory, encoding: 'utf8' }).trim()
  git('init', '--quiet')
  const commitAll = () => {
    git('add', '.')
    git('-c', 'user.name=Release Test', '-c', 'user.email=release@example.invalid', 'commit', '--quiet', '-m', 'fixture')
    return git('rev-parse', 'HEAD')
  }

  const file = path.join(directory, 'deploy/installations/data-compatibility.json')
  await mkdir(path.dirname(file), { recursive: true })
  const writeContract = async value => writeFile(file, JSON.stringify(value))
  return {
    directory,
    git,
    commitAll,
    writeContract,
  }
}

const automaticContract = {
  schema: 2,
  epoch: 9,
  databaseVersion: 8,
  automaticMigration: {
    fromEpochs: [3, 7, 8],
    previousDatabaseVersion: 7,
    plan: 'docs/migration.md',
    tests: ['migrates_old_accounts', 'refuses_newer_data'],
  },
}

async function automaticRepository(previous = { schema: 1, epoch: 3 }) {
  const fixture = await compatibilityRepository()
  await fixture.writeContract(previous)
  const before = fixture.commitAll()
  await mkdir(path.join(fixture.directory, 'docs'))
  await writeFile(path.join(fixture.directory, automaticContract.automaticMigration.plan), 'Migration and forward recovery\n')
  return { ...fixture, before }
}

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
    const {
      directory,
      git,
      commitAll,
      writeContract,
    } = await compatibilityRepository()
    await writeFile(path.join(directory, 'vault.rs'), 'legacy format\n')
    const legacy = commitAll()
    const legacyTree = git('rev-parse', `${legacy}^{tree}`)
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
    await expect(verifyInstallationCompatibility(incompatible, compatible, { cwd: directory })).rejects.toThrow('downgrade refused')
    await expect(verifyInstallationCompatibility(legacy, incompatible, { cwd: directory })).rejects.toThrow('Unknown')

    await writeContract({ schema: 1, epoch: 1, legacyTree: '0'.repeat(40) })
    const unknown = commitAll()
    await expect(verifyInstallationCompatibility(legacy, unknown, { cwd: directory })).rejects.toThrow('Unknown')
    await writeContract({ schema: 1, epoch: 0, legacyTree })
    const invalid = commitAll()
    await expect(verifyInstallationCompatibility(adoption, invalid, { cwd: directory })).rejects.toThrow('Invalid')
  })

  it('automatically accepts only explicitly reviewed forward baselines and refuses downgrades', async () => {
    for (const epoch of [3, 7, 8]) {
      const f = await automaticRepository({ schema: 1, epoch })
      await f.writeContract(automaticContract)
      const after = f.commitAll()
      await expect(verifyInstallationCompatibility(f.before, after, { cwd: f.directory })).resolves.toBeUndefined()
      await expect(verifyInstallationCompatibility(after, f.before, { cwd: f.directory })).rejects.toThrow('downgrade refused')
      await writeFile(path.join(f.directory, 'patch.txt'), 'compatible patch')
      const patch = f.commitAll()
      await expect(verifyInstallationCompatibility(after, patch, { cwd: f.directory })).resolves.toBeUndefined()
    }
  })

  it('refuses unreviewed baselines, a changed database fence and missing committed plans', async () => {
    const f = await automaticRepository({ schema: 1, epoch: 4 })
    await f.writeContract(automaticContract)
    const after = f.commitAll()
    await expect(verifyInstallationCompatibility(f.before, after, { cwd: f.directory })).rejects.toThrow('reviewed forward migration')
    await f.writeContract({ ...automaticContract, databaseVersion: 9 })
    const changedFence = f.commitAll()
    await expect(verifyInstallationCompatibility(after, changedFence, { cwd: f.directory })).rejects.toThrow('without advancing')
    await f.writeContract({
      ...automaticContract,
      epoch: 10,
      databaseVersion: 9,
      automaticMigration: { ...automaticContract.automaticMigration, fromEpochs: [9] },
    })
    const wrongPreviousFence = f.commitAll()
    await expect(verifyInstallationCompatibility(after, wrongPreviousFence, { cwd: f.directory })).rejects.toThrow('reviewed forward migration')
    await f.writeContract({
      ...automaticContract,
      epoch: 10,
      databaseVersion: 9,
      automaticMigration: { ...automaticContract.automaticMigration, fromEpochs: [9], previousDatabaseVersion: 8 },
    })
    await expect(verifyInstallationCompatibility(after, f.commitAll(), { cwd: f.directory })).resolves.toBeUndefined()

    const missing = await automaticRepository()
    await missing.writeContract({ ...automaticContract, automaticMigration: { ...automaticContract.automaticMigration, plan: 'docs/missing.md' } })
    await expect(verifyInstallationCompatibility(missing.before, missing.commitAll(), { cwd: missing.directory })).rejects.toThrow()
  })

  it('requires a valid recovery plan, monotonic database version and named regression coverage', async () => {
    const f = await automaticRepository()
    for (const automaticMigration of [
      { ...automaticContract.automaticMigration, fromEpochs: [3, 3] },
      { ...automaticContract.automaticMigration, fromEpochs: [9] },
      { ...automaticContract.automaticMigration, fromEpochs: [] },
      { ...automaticContract.automaticMigration, previousDatabaseVersion: 8 },
      { ...automaticContract.automaticMigration, plan: 'docs/../secret.md' },
      { ...automaticContract.automaticMigration, tests: [] },
      { ...automaticContract.automaticMigration, tests: ['../command'] },
    ]) {
      await f.writeContract({ ...automaticContract, automaticMigration })
      await expect(verifyInstallationCompatibility(f.before, f.commitAll(), { cwd: f.directory })).rejects.toThrow('Invalid')
    }
  })

  it('qualifies actual passing tests and rejects missing, ignored or failed regression evidence', async () => {
    const f = await automaticRepository()
    await f.writeContract(automaticContract)
    const passing = 'test migrates_old_accounts ... ok\ntest refuses_newer_data ... ok\n'
    await expect(qualifyInstallationMigrations(passing, { cwd: f.directory })).resolves.toBeUndefined()
    for (const log of ['', passing.replace('refuses_newer_data', 'different_test'), passing.replace('... ok', '... ignored'), passing.replace('... ok', '... FAILED')])
      await expect(qualifyInstallationMigrations(log, { cwd: f.directory })).rejects.toThrow('passing backend regression evidence')
    await writeFile(path.join(f.directory, automaticContract.automaticMigration.plan), '')
    await expect(qualifyInstallationMigrations(passing, { cwd: f.directory })).rejects.toThrow('recovery plan')
  })

  it('waits through an updater replacement and requires the manager, runner, and execution health', async () => {
    const ssh = vi.fn().mockResolvedValueOnce(Buffer.from('{"ready":false}')).mockResolvedValueOnce(Buffer.from('{"ready":true,"manager":true,"runner":true,"health":true}'))
    const sleep = vi.fn()
    expect(await verifyInstallation({ installationImage: image }, { ssh, sleep, now: () => now })).toMatchObject({ ready: true })
    expect(sleep).toHaveBeenCalledOnce()
  })

  it('does not accept an unhealthy or mismatched installation after migration', async () => {
    for (const missing of ['ready', 'manager', 'runner', 'health']) {
      let clock = 0
      const result = {
        ready: true,
        manager: true,
        runner: true,
        health: true,
        [missing]: false,
      }
      await expect(verifyInstallation({ installationImage: image }, {
        ssh: vi.fn().mockResolvedValue(Buffer.from(JSON.stringify(result))),
        sleep: vi.fn(),
        now: () => clock++,
        timeoutMs: 2,
      })).rejects.toThrow('did not become healthy')
    }
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
    const quality = workflow.jobs.quality.steps.map(step => step.run ?? '').join('\n')
    expect(quality).toContain('set -o pipefail')
    expect(quality).toContain('node scripts/test-backend.mjs --features ublk-prototype 2>&1 | tee backend-tests.log')
    expect(quality.indexOf('qualify backend-tests.log')).toBeGreaterThan(quality.indexOf('tee backend-tests.log'))
  })
})
