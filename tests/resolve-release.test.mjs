import { writeFile } from 'node:fs/promises'
import path from 'node:path'
import { describe, expect, it } from 'vitest'
import {
  ImageValidationPendingError,
  resolveRelease,
  trustedRun,
  verifiedImage,
} from '../scripts/resolve-release.mjs'

const config = { repository: 'leo91000/leo-agent-manager', commit: 'a'.repeat(40) }
const digest = `sha256:${'b'.repeat(64)}`
const run = {
  id: 123,
  event: 'push',
  head_branch: 'main',
  head_sha: config.commit,
  head_repository: { full_name: config.repository },
  path: '.github/workflows/ci.yaml',
  status: 'completed',
  conclusion: 'success',
}
const evidence = {
  schema: 1,
  repository: config.repository,
  commit: config.commit,
  digest,
  runId: run.id,
}

describe('release validation reuse', () => {
  it('accepts only this workflow on main at the exact commit and repository', () => {
    expect(trustedRun(run, config)).toBe(true)
    for (const mutation of [{ event: 'pull_request' }, { head_sha: 'c'.repeat(40) }, { head_branch: 'other' }, { head_repository: { full_name: 'other/repo' } }, { path: '.github/workflows/other.yaml' }])
      expect(trustedRun({ ...run, ...mutation }, config)).toBe(false)
    expect(verifiedImage(evidence, config, run.id)).toBe(digest)
    for (const mutation of [{ schema: 2 }, { commit: 'c'.repeat(40) }, { digest: 'latest' }, { repository: 'other/repo' }, { runId: 999 }])
      expect(() => verifiedImage({ ...evidence, ...mutation }, config, run.id)).toThrow()
  })
  it('waits for a concurrent main run and verifies its immutable artifact', async () => {
    let requests = 0
    const result = await resolveRelease(config, {
      gh: async (args) => {
        if (args[0] === 'api')
          return JSON.stringify({ workflow_runs: [{ ...run, status: ++requests === 1 ? 'in_progress' : 'completed' }] })
        await writeFile(path.join(args.at(-1), 'image.json'), JSON.stringify(evidence))
        return ''
      },
      sleep: async () => {},
    })
    expect(requests).toBe(2)
    expect(result).toEqual({ digest, runId: run.id })
  })
  it('never reuses failed, foreign, missing, or mismatched validation', async () => {
    for (const runs of [[], [{ ...run, conclusion: 'failure' }], [{ ...run, head_sha: 'c'.repeat(40) }]]) {
      expect(await resolveRelease(config, {
        gh: async () => JSON.stringify({ workflow_runs: runs }),
        sleep: async () => {},
      })).toBeNull()
    }

    await expect(resolveRelease(config, {
      gh: async (args) => {
        if (args[0] === 'api')
          return JSON.stringify({ workflow_runs: [run] })
        await writeFile(path.join(args.at(-1), 'image.json'), JSON.stringify({ ...evidence, commit: 'c'.repeat(40) }))
        return ''
      },
    })).rejects.toThrow('does not match')
  })
  it('does not start a duplicate image build when main is still pending at the deadline', async () => {
    let time = 0
    await expect(resolveRelease(config, {
      gh: async () => JSON.stringify({ workflow_runs: [{ ...run, status: 'in_progress', conclusion: null }] }),
      now: () => time,
      sleep: async () => { time += 10000 },
      timeoutMs: 10000,
    })).rejects.toBeInstanceOf(ImageValidationPendingError)
  })
})
