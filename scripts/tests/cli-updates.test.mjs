import { spawnSync } from 'node:child_process'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'
import { buildVersions } from '../cli-updates.mjs'

describe('application image tool versions', () => {
  const releases = {
    codex: { version: '0.159.0' },
    gh: { tag_name: 'v2.101.0', draft: false, prerelease: false },
  }
  const request = async url => url.includes('registry.npmjs.org') ? releases.codex : releases.gh

  it('resolves the latest stable tools without Dockerfile version defaults', async () => {
    expect(await buildVersions({ request })).toEqual({ codex: '0.159.0', gh: '2.101.0' })
    const dockerfile = readFileSync(new URL('../../Dockerfile', import.meta.url), 'utf8')
    expect(dockerfile).toMatch(/^ARG CODEX_VERSION$/m)
    expect(dockerfile).toMatch(/^ARG GH_VERSION$/m)
  })

  it('passes the resolved exact versions into the CI image that is smoke-tested', () => {
    const jobs = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8')).jobs
    const steps = jobs.image.steps
    const resolve = jobs.resolve.steps.find(step => step.id === 'tools')
    const build = steps.find(step => step.id === 'build')
    expect(resolve.run).toBe('node scripts/cli-updates.mjs build-versions')
    // eslint-disable-next-line no-template-curly-in-string -- GitHub Actions resolves these expressions.
    expect(build.with['build-args']).toContain('CODEX_VERSION=${{ needs.resolve.outputs.codex }}\nGH_VERSION=${{ needs.resolve.outputs.gh }}')
    // eslint-disable-next-line no-template-curly-in-string -- GitHub Actions resolves these expressions.
    expect(jobs.resolve.outputs.codex).toBe('${{ steps.tools.outputs.codex }}')
    // eslint-disable-next-line no-template-curly-in-string -- GitHub Actions resolves these expressions.
    expect(jobs.resolve.outputs.gh).toBe('${{ steps.tools.outputs.gh }}')
    const release = jobs.resolve.steps.find(step => step.id === 'resolve')
    expect(release.env.CODEX_VERSION).toBe(jobs.resolve.outputs.codex)
    expect(release.env.GH_VERSION).toBe(jobs.resolve.outputs.gh)
    expect(jobs.resolve.steps.indexOf(resolve)).toBeLessThan(jobs.resolve.steps.indexOf(release))
    const record = jobs.publish.steps.find(step => step.name === 'Record the validated image')
    expect(record.run).toContain('tools:')
    // eslint-disable-next-line no-template-curly-in-string -- GitHub Actions resolves this expression.
    expect(record.env.CODEX_VERSION).toBe('${{ needs.resolve.outputs.codex }}')
    // eslint-disable-next-line no-template-curly-in-string -- GitHub Actions resolves this expression.
    expect(record.env.GH_VERSION).toBe('${{ needs.resolve.outputs.gh }}')
  })

  it('authenticates only GitHub discovery and keeps the npm request public', async () => {
    const requests = []
    await buildVersions({
      githubToken: 'fixture-token',
      request: async (url, options) => {
        requests.push({ url, options })
        return request(url)
      },
    })
    expect(requests).toEqual([
      { url: 'https://registry.npmjs.org/@openai/codex/latest', options: undefined },
      {
        url: 'https://api.github.com/repos/cli/cli/releases/latest',
        options: { headers: { Accept: 'application/vnd.github+json', authorization: 'Bearer fixture-token' } },
      },
    ])
  })

  it('rejects unstable releases and failed discovery before building', async () => {
    await expect(buildVersions({
      request: async () => {
        throw new Error('Registry unavailable')
      },
    })).rejects.toThrow('Registry unavailable')
    await expect(buildVersions({ request: async url => url.includes('registry.npmjs.org') ? { version: '0.159.0-beta' } : releases.gh })).rejects.toThrow(/stable/)
    for (const flag of ['draft', 'prerelease']) {
      await expect(buildVersions({ request: async url => url.includes('registry.npmjs.org') ? releases.codex : { ...releases.gh, [flag]: true } })).rejects.toThrow(/not stable/)
    }
  })
})

describe('build-versions command', () => {
  const preload = `globalThis.fetch = async (url) => {
    const release = url === 'https://registry.npmjs.org/@openai/codex/latest'
      ? { version: '0.159.0' }
      : { tag_name: 'v2.101.0', draft: false, prerelease: false };
    return Response.json(release);
  };`
  const run = (args, env = {}, setup = preload) => spawnSync(process.execPath, [
    '--import',
    `data:text/javascript,${encodeURIComponent(setup)}`,
    'scripts/cli-updates.mjs',
    ...args,
  ], { encoding: 'utf8', env: { ...process.env, ...env }, timeout: 10000 })

  it('emits exact JSON versions and appends the existing GitHub Actions outputs', () => {
    const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-build-versions-'))
    try {
      const output = path.join(directory, 'outputs')
      writeFileSync(output, 'existing=value\n')
      const result = run(['build-versions'], { GITHUB_OUTPUT: output })
      expect(result.status).toBe(0)
      expect(result.stderr).toBe('')
      expect(JSON.parse(result.stdout)).toEqual({ codex: '0.159.0', gh: '2.101.0' })
      expect(readFileSync(output, 'utf8')).toBe('existing=value\ncodex=0.159.0\ngh=2.101.0\n')
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it('fails discovery without emitting usable build outputs', () => {
    const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-build-versions-'))
    try {
      const output = path.join(directory, 'outputs')
      writeFileSync(output, 'existing=value\n')
      const result = run(['build-versions'], { GITHUB_OUTPUT: output }, 'globalThis.fetch = async () => new Response("unavailable", { status: 503 });')
      expect(result.status).toBe(1)
      expect(result.stdout).toBe('')
      expect(result.stderr.trim()).toBe('Request failed (HTTP 503).')
      expect(readFileSync(output, 'utf8')).toBe('existing=value\n')
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it('rejects removed deployment commands before making any network request', () => {
    for (const args of [[], ['check'], ['deploy']]) {
      const result = run(args, {}, 'globalThis.fetch = () => { throw new Error("Unexpected network request"); };')
      expect(result.status).toBe(1)
      expect(result.stdout).toBe('')
      expect(result.stderr.trim()).toBe('Usage: cli-updates.mjs build-versions')
    }
  })
})
