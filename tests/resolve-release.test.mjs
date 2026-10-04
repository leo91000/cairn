import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import {
  evidenceArtifact,
  ImageValidationPendingError,
  resolveRelease,
  trustedRun,
  verifiedImage,
} from '../scripts/resolve-release.mjs'

const repository = 'leo91000/leo-agent-manager'
const tree = 'e'.repeat(40)
const config = {
  repository,
  commit: 'a'.repeat(40),
  tree,
  tools: { codex: '0.159.0', gh: '2.101.0' },
}
const digest = `sha256:${'b'.repeat(64)}`
// A pull request run validates GitHub's merge commit, whose tree becomes main's tree.
const pullRequestCommit = 'd'.repeat(40)
const pullRequestRun = {
  id: 456,
  event: 'pull_request',
  head_branch: 'perf/fast-ci',
  head_sha: 'f'.repeat(40),
  head_repository: { full_name: repository },
  path: '.github/workflows/ci.yaml',
  status: 'completed',
  conclusion: 'success',
}
const mainRun = {
  ...pullRequestRun,
  id: 123,
  event: 'push',
  head_branch: 'main',
  head_sha: config.commit,
}

function evidenceFor(run, commit, changes = {}) {
  return {
    schema: 2,
    repository,
    tree,
    commit,
    digest,
    runId: run.id,
    tools: config.tools,
    ...changes,
  }
}

// Serves the GitHub API calls made by the resolver from in-memory runs and evidence.
function github({ runs, evidence = {}, mainRuns = () => [] }) {
  return async (args) => {
    if (args[0] === 'run' && args[1] === 'download') {
      expect(args).toContain(evidenceArtifact(tree))
      await writeFile(path.join(args.at(-1), 'image.json'), JSON.stringify(evidence[args[2]]))
      return ''
    }

    const endpoint = args[1]
    if (endpoint.startsWith(`repos/${repository}/actions/artifacts?`)) {
      expect(new URLSearchParams(endpoint.split('?')[1]).get('name')).toBe(evidenceArtifact(tree))
      const artifacts = runs().filter(run => evidence[run.id]).map(run => ({ name: evidenceArtifact(tree), expired: false, workflow_run: { id: run.id } }))
      return JSON.stringify({ artifacts })
    }

    const runId = endpoint.match(/actions\/runs\/(\d+)$/)?.[1]
    if (runId)
      return JSON.stringify(runs().find(run => run.id === Number(runId)))
    if (endpoint.startsWith(`repos/${repository}/actions/workflows/ci.yaml/runs?`))
      return JSON.stringify({ workflow_runs: mainRuns() })
    throw new Error(`Unexpected GitHub call: ${args.join(' ')}`)
  }
}

describe('release validation reuse', () => {
  it('trusts this workflow on main or on a pull request from this repository', () => {
    expect(trustedRun(mainRun, config)).toBe(true)
    expect(trustedRun(pullRequestRun, config)).toBe(true)
    for (const mutation of [{ event: 'workflow_dispatch' }, { head_branch: 'other' }, { head_repository: { full_name: 'other/repo' } }, { path: '.github/workflows/other.yaml' }])
      expect(trustedRun({ ...mainRun, ...mutation }, config)).toBe(false)
    expect(trustedRun({ ...pullRequestRun, head_repository: { full_name: 'fork/leo-agent-manager' } }, config)).toBe(false)
  })

  it('verifies evidence for the exact tree, run, repository and image digest', () => {
    const evidence = evidenceFor(pullRequestRun, pullRequestCommit)
    expect(verifiedImage(evidence, config, pullRequestRun.id)).toEqual({ digest, commit: pullRequestCommit })
    for (const mutation of [{ schema: 1 }, { tree: 'c'.repeat(40) }, { commit: 'main' }, { digest: 'latest' }, { repository: 'other/repo' }, { runId: 999 }])
      expect(() => verifiedImage({ ...evidence, ...mutation }, config, pullRequestRun.id)).toThrow()
  })

  it('reuses the image a pull request validated for the identical merged tree', async () => {
    const result = await resolveRelease(config, {
      gh: github({ runs: () => [pullRequestRun], evidence: { [pullRequestRun.id]: evidenceFor(pullRequestRun, pullRequestCommit) } }),
      wait: false,
    })

    // The image keeps the commit it was built from; deployment verifies that commit.
    expect(result).toEqual({ digest, commit: pullRequestCommit, runId: pullRequestRun.id })
  })

  it('never reuses failed, unfinished or forked validation', async () => {
    for (const run of [{ ...pullRequestRun, conclusion: 'failure' }, { ...pullRequestRun, status: 'in_progress', conclusion: null }, { ...pullRequestRun, head_repository: { full_name: 'fork/leo-agent-manager' } }]) {
      expect(await resolveRelease(config, {
        gh: github({ runs: () => [run], evidence: { [run.id]: evidenceFor(run, pullRequestCommit) } }),
        wait: false,
      })).toBeNull()
    }
  })

  it('rebuilds if the validated image has older or unrecorded tools', async () => {
    for (const tools of [undefined, { ...config.tools, codex: '0.156.1' }, { ...config.tools, gh: '2.100.0' }]) {
      expect(await resolveRelease(config, {
        gh: github({ runs: () => [mainRun], evidence: { [mainRun.id]: evidenceFor(mainRun, config.commit, { tools }) } }),
        wait: false,
      })).toBeNull()
    }
  })

  it('rejects evidence that contradicts its artifact', async () => {
    await expect(resolveRelease(config, {
      gh: github({ runs: () => [mainRun], evidence: { [mainRun.id]: evidenceFor(mainRun, config.commit, { tree: 'c'.repeat(40) }) } }),
      wait: false,
    })).rejects.toThrow('does not match')
  })

  it('lets a tag wait for its concurrent main run, then reuse that validation', async () => {
    let polls = 0
    const finished = () => polls >= 2
    const result = await resolveRelease(config, {
      gh: github({
        runs: () => finished() ? [mainRun] : [],
        evidence: { [mainRun.id]: evidenceFor(mainRun, config.commit) },
        mainRuns: () => [{ ...mainRun, status: finished() ? 'completed' : 'in_progress' }],
      }),
      sleep: async () => { polls++ },
      wait: true,
    })

    expect(polls).toBe(2)
    expect(result).toEqual({ digest, commit: config.commit, runId: mainRun.id })
  })

  it('starts a full validation on main instead of waiting for other runs', async () => {
    const result = await resolveRelease(config, {
      gh: github({ runs: () => [], mainRuns: () => [{ ...mainRun, status: 'in_progress', conclusion: null }] }),
      sleep: async () => { throw new Error('main must not wait') },
      wait: false,
    })

    expect(result).toBeNull()
  })

  it('does not start a duplicate image build when main is still pending at the deadline', async () => {
    let time = 0
    await expect(resolveRelease(config, {
      gh: github({ runs: () => [], mainRuns: () => [{ ...mainRun, status: 'in_progress', conclusion: null }] }),
      now: () => time,
      sleep: async () => { time += 10000 },
      timeoutMs: 10000,
      wait: true,
    })).rejects.toBeInstanceOf(ImageValidationPendingError)
  })
})
