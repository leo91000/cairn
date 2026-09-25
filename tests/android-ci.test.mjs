import {
  mkdtemp,
  readFile,
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
} from 'vitest'
import { artifactName, recordBuild, verifyBuild } from '../scripts/android-build.mjs'
import { androidVersion } from '../scripts/android-release.mjs'
import { AndroidValidationPendingError, resolveAndroidRelease, trustedAndroidRun } from '../scripts/resolve-android-release.mjs'

const config = {
  repository: 'leo91000/leo-agent-manager',
  commit: 'a'.repeat(40),
  tag: 'v0.31.0',
  runId: 123,
}
const run = {
  id: config.runId,
  event: 'push',
  head_branch: 'main',
  head_sha: config.commit,
  head_repository: { full_name: config.repository },
  path: '.github/workflows/android.yaml',
  status: 'completed',
  conclusion: 'success',
}
const directories = []

async function fixture(directory, context = config) {
  await writeFile(path.join(directory, 'app-release-unsigned.apk'), 'compiled APK bytes')
  await writeFile(path.join(directory, 'output-metadata.json'), JSON.stringify({
    applicationId: 'dev.leo.manager',
    elements: [{ ...androidVersion(context.tag), outputFile: 'app-release-unsigned.apk' }],
  }))
  return recordBuild(directory, context)
}

async function temporaryBuild() {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'android-ci-test-'))
  directories.push(directory)
  await fixture(directory)
  return directory
}

afterEach(async () => {
  await Promise.all(directories.splice(0).map(directory => rm(directory, { recursive: true, force: true })))
})

describe('validated Android build reuse', () => {
  it('binds the compiled APK to its repository, commit, run and release version', async () => {
    const directory = await temporaryBuild()
    expect(await verifyBuild(directory, config)).toMatchObject({ commit: config.commit, runId: 123, versionName: '0.31.0' })
    for (const change of [{ repository: 'other/repo' }, { commit: 'b'.repeat(40) }, { runId: 124 }, { tag: 'v0.32.0' }])
      await expect(verifyBuild(directory, { ...config, ...change })).rejects.toThrow()
  })

  it('refuses altered APK bytes, metadata or evidence before signing', async () => {
    const directory = await temporaryBuild()
    await writeFile(path.join(directory, 'app-release-unsigned.apk'), 'modified APK bytes')
    await expect(verifyBuild(directory, config)).rejects.toThrow('evidence')
    await fixture(directory)
    const metadataPath = path.join(directory, 'output-metadata.json')
    const metadata = JSON.parse(await readFile(metadataPath, 'utf8'))
    await writeFile(metadataPath, JSON.stringify({ ...metadata, applicationId: 'another.app' }))
    await expect(verifyBuild(directory, config)).rejects.toThrow()
    for (const change of [{ schema: 2 }, { size: 0 }, { sha256: 'b'.repeat(64) }, { versionCode: 1 }]) {
      const evidence = await fixture(directory)
      await writeFile(path.join(directory, 'validation.json'), JSON.stringify({ ...evidence, ...change }))
      await expect(verifyBuild(directory, config)).rejects.toThrow('evidence')
    }
  })

  it('accepts validation only from the Android push workflow on main at the exact commit', () => {
    expect(trustedAndroidRun(run, config)).toBe(true)
    for (const change of [{ event: 'pull_request' }, { event: 'workflow_dispatch' }, { head_branch: 'other' }, { head_sha: 'b'.repeat(40) }, { path: '.github/workflows/ci.yaml' }, { head_repository: { full_name: 'other/repo' } }])
      expect(trustedAndroidRun({ ...run, ...change }, config)).toBe(false)
  })

  it('waits for concurrent main validation and verifies its artifact before skipping any checks', async () => {
    let queries = 0
    let sleeps = 0
    const result = await resolveAndroidRelease(config, {
      gh: async (args) => {
        if (args[0] === 'api')
          return JSON.stringify({ workflow_runs: [{ ...run, status: ++queries === 1 ? 'in_progress' : 'completed' }] })
        expect(args.slice(0, -1)).toEqual(['run', 'download', '123', '--repo', config.repository, '--name', artifactName, '--dir'])
        await fixture(args.at(-1))
        return ''
      },
      sleep: async () => { sleeps++ },
    })
    expect(result).toEqual({ runId: 123 })
    expect(sleeps).toBe(1)
  })

  it('allows an atomic main and tag push to become discoverable', async () => {
    let queries = 0
    expect(await resolveAndroidRelease(config, {
      gh: async (args) => {
        if (args[0] === 'api')
          return JSON.stringify({ workflow_runs: ++queries === 1 ? [] : [run] })
        await fixture(args.at(-1))
        return ''
      },
      sleep: async () => {},
    })).toEqual({ runId: 123 })
  })

  it.each(['failure', 'cancelled', 'timed_out'])('does not reuse a %s main run', async (conclusion) => {
    expect(await resolveAndroidRelease(config, {
      gh: async () => JSON.stringify({ workflow_runs: [{ ...run, conclusion }] }),
    })).toBeNull()
  })

  it('falls back to full validation when there is no trusted main run', async () => {
    for (const runs of [[], [{ ...run, head_branch: 'other' }]]) {
      expect(await resolveAndroidRelease(config, {
        gh: async () => JSON.stringify({ workflow_runs: runs }),
        sleep: async () => {},
      })).toBeNull()
    }
  })

  it('cannot reuse expired artifacts or an APK built for a different version', async () => {
    await expect(resolveAndroidRelease(config, {
      gh: async (args) => {
        if (args[0] === 'api')
          return JSON.stringify({ workflow_runs: [run] })
        throw new Error('artifact expired')
      },
    })).rejects.toThrow('artifact expired')
    await expect(resolveAndroidRelease(config, {
      gh: async (args) => {
        if (args[0] === 'api')
          return JSON.stringify({ workflow_runs: [run] })
        await fixture(args.at(-1), { ...config, tag: 'v0.30.0' })
        return ''
      },
    })).rejects.toThrow('does not match')
  })

  it('does not launch duplicate validation when main is still pending at the deadline', async () => {
    let time = 0
    await expect(resolveAndroidRelease(config, {
      gh: async () => JSON.stringify({ workflow_runs: [{ ...run, status: 'in_progress', conclusion: null }] }),
      now: () => time,
      sleep: async () => { time += 10000 },
      timeoutMs: 10000,
    })).rejects.toBeInstanceOf(AndroidValidationPendingError)
  })
})
