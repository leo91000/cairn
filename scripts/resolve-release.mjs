import { execFile } from 'node:child_process'
import {
  appendFile,
  mkdtemp,
  readFile,
  rm,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const artifactName = 'validated-image'

export class ImageValidationPendingError extends Error {}

export function trustedRun(run, { repository, commit }) {
  return run.event === 'push'
    && run.head_branch === 'main'
    && run.head_sha === commit
    && run.head_repository?.full_name?.toLowerCase() === repository.toLowerCase()
    && run.path === '.github/workflows/ci.yaml'
}

export function verifiedImage(value, { repository, commit }, runId) {
  if (value.schema !== 1 || value.commit !== commit || value.runId !== runId
    || value.repository !== repository.toLowerCase()
    || !/^sha256:[a-f0-9]{64}$/.test(value.digest)) {
    throw new Error('Image evidence does not match this release')
  }

  return value.digest
}

export async function resolveRelease(config, {
  gh,
  sleep = setTimeout,
  now = Date.now,
  timeoutMs = 40 * 60 * 1000,
} = {}) {
  const deadline = now() + timeoutMs
  const query = new URLSearchParams({
    head_sha: config.commit,
    branch: 'main',
    event: 'push',
    per_page: '30',
  })
  let discoveryAttempts = 0
  while (now() < deadline) {
    const response = await gh(['api', `repos/${config.repository}/actions/workflows/ci.yaml/runs?${query}`])
    const runs = JSON.parse(response).workflow_runs.filter(run => trustedRun(run, config))
    const successful = runs.find(run => run.status === 'completed' && run.conclusion === 'success')
    if (successful) {
      const directory = await mkdtemp(path.join(os.tmpdir(), 'leo-release-'))
      try {
        await gh(['run', 'download', successful.id.toString(), '--repo', config.repository, '--name', artifactName, '--dir', directory])
        const evidence = JSON.parse(await readFile(path.join(directory, 'image.json'), 'utf8'))
        return { digest: verifiedImage(evidence, config, successful.id), runId: successful.id }
      }
      finally {
        await rm(directory, { recursive: true, force: true })
      }
    }

    if (!runs.some(run => run.status !== 'completed')) {
      // Give an atomic main+tag push a short discovery window. A tag-only commit
      // or a failed main run falls back to the complete pipeline.
      if (runs.length || ++discoveryAttempts >= 3)
        return null
    }

    await sleep(runs.length ? 10000 : 5000)
  }

  // Main's image job can take up to 35 minutes. Do not spend 15 minutes
  // waiting and then launch another full build of the same pending commit.
  throw new ImageValidationPendingError('Timed out waiting for image validation on main; retry after it finishes')
}

if (import.meta.main) {
  const config = { repository: process.env.GITHUB_REPOSITORY, commit: process.env.GITHUB_SHA }
  let result = null
  if (process.env.GITHUB_REF_TYPE === 'tag') {
    try {
      result = await resolveRelease(config, {
        gh: async args => (await exec('gh', args, { timeout: 30000, maxBuffer: 2 * 1024 * 1024 })).stdout,
      })
    }
    catch (error) {
      if (error instanceof ImageValidationPendingError)
        throw error
      console.log('Previous validation is unavailable; running all checks and a fresh image build.')
    }
  }

  const output = `reuse=${!!result}\ndigest=${result?.digest || ''}\n`
  await appendFile(process.env.GITHUB_OUTPUT, output)
  const summary = result
    ? `Reusing the verified image for ${config.commit} from https://github.com/${config.repository}/actions/runs/${result.runId}.\n`
    : 'This commit will run the complete validation and image pipeline.\n'
  console.log(summary)
  if (process.env.GITHUB_STEP_SUMMARY)
    await appendFile(process.env.GITHUB_STEP_SUMMARY, summary)
}
