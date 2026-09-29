import { execFile } from 'node:child_process'
import { appendFile, mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'
import { artifactName, verifyBuild } from './android-build.mjs'
import { androidVersion } from './android-release.mjs'

const exec = promisify(execFile)

export class AndroidValidationPendingError extends Error {}

export function trustedAndroidRun(run, config) {
  return run.event === 'push' && run.head_branch === 'main'
    && run.head_sha === config.commit
    && run.head_repository?.full_name?.toLowerCase() === config.repository.toLowerCase()
    && run.path === '.github/workflows/android.yaml'
}

export async function resolveAndroidRelease(config, {
  gh,
  sleep = setTimeout,
  now = Date.now,
  timeoutMs = 45 * 60 * 1000,
} = {}) {
  androidVersion(config.tag)
  const deadline = now() + timeoutMs
  const query = new URLSearchParams({
    head_sha: config.commit,
    branch: 'main',
    event: 'push',
    per_page: '30',
  })
  let discoveryAttempts = 0
  while (now() < deadline) {
    const response = await gh(['api', `repos/${config.repository}/actions/workflows/android.yaml/runs?${query}`])
    const runs = JSON.parse(response).workflow_runs.filter(run => trustedAndroidRun(run, config))
    const successful = runs.find(run => run.status === 'completed' && run.conclusion === 'success')
    if (successful) {
      const directory = await mkdtemp(path.join(os.tmpdir(), 'leo-android-release-'))
      try {
        await gh(['run', 'download', successful.id.toString(), '--repo', config.repository, '--name', artifactName, '--dir', directory])
        await verifyBuild(directory, { ...config, runId: successful.id })
        return { runId: successful.id }
      }
      finally {
        await rm(directory, { recursive: true, force: true })
      }
    }

    if (!runs.some(run => run.status !== 'completed')) {
      // A tag-only commit, failed main run or missing Android path trigger still
      // gets the complete pipeline. Allow discovery of an atomic main+tag push.
      if (runs.length || ++discoveryAttempts >= 3)
        return null
    }

    await sleep(runs.length ? 10000 : 5000)
  }

  // Do not start a second full build while main may still be validating it.
  throw new AndroidValidationPendingError('Timed out waiting for Android validation on main; retry after it finishes')
}

if (import.meta.main) {
  let result = null
  if (process.env.GITHUB_REF_TYPE === 'tag') {
    const config = { repository: process.env.GITHUB_REPOSITORY, commit: process.env.GITHUB_SHA, tag: process.env.GITHUB_REF_NAME }
    androidVersion(config.tag)
    try {
      result = await resolveAndroidRelease(config, {
        gh: async args => (await exec('gh', args, { timeout: 30000, maxBuffer: 2 * 1024 * 1024 })).stdout,
      })
    }
    catch (error) {
      if (error instanceof AndroidValidationPendingError)
        throw error
      console.log('Reusable Android build is unavailable or incompatible; running the complete validation pipeline.')
    }
  }

  await appendFile(process.env.GITHUB_OUTPUT, `reuse=${!!result}\nrun-id=${result?.runId || ''}\n`)
  const summary = result
    ? `Reusing the verified Android APK for ${process.env.GITHUB_SHA} from https://github.com/${process.env.GITHUB_REPOSITORY}/actions/runs/${result.runId}. Only signing and publication remain.\n`
    : 'This commit will run the complete Android validation and build pipeline.\n'
  console.log(summary)
  if (process.env.GITHUB_STEP_SUMMARY)
    await appendFile(process.env.GITHUB_STEP_SUMMARY, summary)
}
