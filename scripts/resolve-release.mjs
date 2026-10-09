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

export class ImageValidationPendingError extends Error {}

// Evidence is keyed by the validated Git tree: a pull request validates GitHub's
// merge commit, whose tree is exactly what main receives when nothing merged since.
export function evidenceArtifact(tree) {
  return `validated-image-${tree}`
}

// Write access is required to push a branch of this repository, as it is to push
// main. Fork pull requests and manual runs can never supply release proof.
export function trustedRun(run, { repository }) {
  const sameRepository = run.head_repository?.full_name?.toLowerCase() === repository.toLowerCase()
  const validation = run.event === 'pull_request' || (run.event === 'push' && run.head_branch === 'main')
  return sameRepository && validation && run.path === '.github/workflows/ci.yaml'
}

export function verifiedImage(value, { repository, tree }, runId) {
  if (value.schema !== 3 || value.tree !== tree || value.runId !== runId
    || value.repository !== repository.toLowerCase()
    || !/^[a-f0-9]{40}$/.test(value.commit)
    || !/^sha256:[a-f0-9]{64}$/.test(value.digest)
    || !/^sha256:[a-f0-9]{64}$/.test(value.beaconDigest)) {
    throw new Error('Image evidence does not match this release')
  }

  return { digest: value.digest, beaconDigest: value.beaconDigest, commit: value.commit }
}

async function validatedImage(config, gh) {
  const name = evidenceArtifact(config.tree)
  const query = new URLSearchParams({ name, per_page: '30' })
  const { artifacts } = JSON.parse(await gh(['api', `repos/${config.repository}/actions/artifacts?${query}`]))

  for (const artifact of artifacts.filter(artifact => artifact.name === name && !artifact.expired)) {
    const run = JSON.parse(await gh(['api', `repos/${config.repository}/actions/runs/${artifact.workflow_run.id}`]))
    if (!trustedRun(run, config) || run.status !== 'completed' || run.conclusion !== 'success')
      continue

    // A run writes its own evidence, so bind it to the head commit GitHub recorded:
    // the validating workflow is then part of the released tree. A pull request
    // qualifies only if its branch already contained main's tip.
    const head = JSON.parse(await gh(['api', `repos/${config.repository}/git/commits/${run.head_sha}`]))
    if (head.tree?.sha !== config.tree)
      continue

    const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-release-'))
    try {
      await gh(['run', 'download', run.id.toString(), '--repo', config.repository, '--name', name, '--dir', directory])
      const evidence = JSON.parse(await readFile(path.join(directory, 'image.json'), 'utf8'))
      // Evidence contradicting its own artifact stops reuse instead of being skipped.
      const image = verifiedImage(evidence, config, run.id)
      const currentTools = !config.tools || Object.entries(config.tools).every(([tool, version]) => evidence.tools?.[tool] === version)
      if (currentTools)
        return { ...image, runId: run.id }
    }
    finally {
      await rm(directory, { recursive: true, force: true })
    }
  }

  return null
}

export async function resolveRelease(config, {
  gh,
  sleep = setTimeout,
  now = Date.now,
  timeoutMs = 40 * 60 * 1000,
  wait = true,
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
    const image = await validatedImage(config, gh)
    if (image || !wait)
      return image

    const response = await gh(['api', `repos/${config.repository}/actions/workflows/ci.yaml/runs?${query}`])
    const runs = JSON.parse(response).workflow_runs.filter(run => trustedRun(run, config) && run.head_sha === config.commit)
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
  if (!process.env.CODEX_VERSION || !process.env.GH_VERSION)
    throw new Error('Missing resolved stable tool versions')
  if (!/^[a-f0-9]{40}$/.test(process.env.GIT_TREE ?? ''))
    throw new Error('Missing the checked-out Git tree')
  const config = {
    repository: process.env.GITHUB_REPOSITORY,
    commit: process.env.GITHUB_SHA,
    tree: process.env.GIT_TREE,
    tools: { codex: process.env.CODEX_VERSION, gh: process.env.GH_VERSION },
  }
  let result = null
  // Pull requests always validate. Main reuses an identical pull request tree;
  // a tag can also wait for the main run of its own commit.
  if (process.env.GITHUB_EVENT_NAME === 'push') {
    try {
      result = await resolveRelease(config, {
        gh: async args => (await exec('gh', args, { timeout: 30000, maxBuffer: 2 * 1024 * 1024 })).stdout,
        wait: process.env.GITHUB_REF_TYPE === 'tag',
      })
    }
    catch (error) {
      if (error instanceof ImageValidationPendingError)
        throw error
      console.log('Previous validation is unavailable; running all checks and a fresh image build.')
    }
  }

  const output = `reuse=${!!result}\ndigest=${result?.digest || ''}\nbeacon-digest=${result?.beaconDigest || ''}\ncommit=${result?.commit || config.commit}\ntree=${config.tree}\nartifact=${evidenceArtifact(config.tree)}\n`
  await appendFile(process.env.GITHUB_OUTPUT, output)
  const summary = result
    ? `Reusing the verified image for tree ${config.tree} from https://github.com/${config.repository}/actions/runs/${result.runId}.\n`
    : 'This commit will run the complete validation and image pipeline.\n'
  console.log(summary)
  if (process.env.GITHUB_STEP_SUMMARY)
    await appendFile(process.env.GITHUB_STEP_SUMMARY, summary)
}
