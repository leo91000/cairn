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
import { buildVersions, engines } from '../cli-updates.mjs'

// Registry responses as observed on 2026-10-24: Claude Code's stable channel
// trails its latest channel, and OpenCode publishes dev snapshots between releases.
const registry = {
  'https://registry.npmjs.org/@openai/codex/latest': { version: '0.159.0' },
  'https://registry.npmjs.org/@anthropic-ai/claude-code/stable': { version: '2.1.308' },
  'https://registry.npmjs.org/@anthropic-ai/claude-code/latest': { version: '2.1.310' },
  'https://registry.npmjs.org/opencode-ai/latest': { version: '1.19.4' },
  'https://api.github.com/repos/cli/cli/releases/latest': { tag_name: 'v2.101.0', draft: false, prerelease: false },
}
const resolved = {
  codex: '0.159.0',
  gh: '2.101.0',
  claude: '2.1.308',
  opencode: '1.19.4',
}

function fixture(changes = {}) {
  const responses = { ...registry, ...changes }
  return async (url) => {
    if (!(url in responses))
      throw new Error(`Unexpected request: ${url}`)
    if (responses[url] instanceof Error)
      throw responses[url]
    return responses[url]
  }
}

describe('application image tool versions', () => {
  it('resolves every agent tool without Dockerfile version defaults', async () => {
    expect(await buildVersions({ request: fixture() })).toEqual(resolved)
    const dockerfile = readFileSync(new URL('../../Dockerfile', import.meta.url), 'utf8')
    for (const name of ['CODEX_VERSION', 'GH_VERSION', 'CLAUDE_VERSION', 'OPENCODE_VERSION']) {
      expect(dockerfile).toMatch(new RegExp(`^ARG ${name}$`, 'm'))
      expect(dockerfile).toContain(`APP_${name}=$${name}`)
    }

    // Each engine is installed at the exact resolved version and checked before validation.
    // eslint-disable-next-line no-template-curly-in-string -- Docker expands these build arguments.
    expect(dockerfile).toContain('test "$(claude --version)" = "${CLAUDE_VERSION} (Claude Code)"')
    // eslint-disable-next-line no-template-curly-in-string -- Docker expands these build arguments.
    expect(dockerfile).toContain('test "$(codex --version)" = "codex-cli ${CODEX_VERSION}"')
    expect(dockerfile).toContain('test "$(opencode --version)" = "$OPENCODE_VERSION"')
  })

  it('follows Claude Code\'s stable channel, never its newer latest channel', async () => {
    const versions = await buildVersions({ request: fixture() })
    expect(versions.claude).toBe('2.1.308')
    expect(engines.claude).toMatchObject({ package: '@anthropic-ai/claude-code', channel: 'stable' })
  })

  it('keeps OpenCode in the series its adapter accepts', () => {
    const adapter = readFileSync(new URL('../../crates/installation/src/opencode/mod.rs', import.meta.url), 'utf8')
    expect(adapter).toContain(`pub const SERIES: &str = "${engines.opencode.series}";`)
  })

  it('passes the resolved exact versions into the CI image that is smoke-tested', () => {
    const jobs = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8')).jobs
    const resolve = jobs.resolve.steps.find(step => step.id === 'tools')
    const release = jobs.resolve.steps.find(step => step.id === 'resolve')
    const record = jobs.publish.steps.find(step => step.name === 'Record the validated image')
    const builds = [jobs.image.steps.find(step => step.id === 'build'), jobs['image-cache'].steps.find(step => step.with?.['build-args']?.includes('CODEX_VERSION'))]
    expect(resolve.run).toBe('node scripts/cli-updates.mjs build-versions')
    expect(jobs.resolve.steps.indexOf(resolve)).toBeLessThan(jobs.resolve.steps.indexOf(release))
    for (const [tool, name] of Object.entries({
      codex: 'CODEX_VERSION',
      gh: 'GH_VERSION',
      claude: 'CLAUDE_VERSION',
      opencode: 'OPENCODE_VERSION',
    })) {
      const output = `\${{ needs.resolve.outputs.${tool} }}`
      expect(jobs.resolve.outputs[tool]).toBe(`\${{ steps.tools.outputs.${tool} }}`)
      expect(release.env[name]).toBe(jobs.resolve.outputs[tool])
      for (const build of builds)
        expect(build.with['build-args']).toContain(`${name}=${output}\n`)
      expect(record.env[name]).toBe(output)
      expect(record.run).toContain(`${tool}: process.env.${name}`)
    }
  })

  it('authenticates only GitHub discovery and keeps npm requests public', async () => {
    const requests = []
    const request = fixture()
    await buildVersions({
      githubToken: 'fixture-token',
      request: async (url, options) => {
        requests.push({ url, options })
        return request(url)
      },
    })
    expect(requests).toEqual(expect.arrayContaining([
      { url: 'https://registry.npmjs.org/@openai/codex/latest', options: undefined },
      { url: 'https://registry.npmjs.org/@anthropic-ai/claude-code/stable', options: undefined },
      { url: 'https://registry.npmjs.org/opencode-ai/latest', options: undefined },
      {
        url: 'https://api.github.com/repos/cli/cli/releases/latest',
        options: { headers: { Accept: 'application/vnd.github+json', authorization: 'Bearer fixture-token' } },
      },
    ]))
    expect(requests).toHaveLength(4)
  })

  it('rejects unstable releases and failed discovery before building', async () => {
    await expect(buildVersions({
      request: async () => {
        throw new Error('Registry unavailable')
      },
    })).rejects.toThrow('Registry unavailable')
    await expect(buildVersions({ request: fixture({ 'https://registry.npmjs.org/@openai/codex/latest': { version: '0.159.0-beta' } }) })).rejects.toThrow('Codex latest channel is not a stable release.')
    for (const flag of ['draft', 'prerelease']) {
      await expect(buildVersions({ request: fixture({ 'https://api.github.com/repos/cli/cli/releases/latest': { ...registry['https://api.github.com/repos/cli/cli/releases/latest'], [flag]: true } }) })).rejects.toThrow(/not stable/)
    }
  })

  it.each([
    ['a prerelease', { version: '2.2.0-beta.1' }, 'Claude Code stable channel is not a stable release.'],
    ['missing', new Error('Request failed (HTTP 404).'), 'Request failed (HTTP 404).'],
    ['malformed', { name: '@anthropic-ai/claude-code' }, 'Claude Code stable channel is not a stable release.'],
    ['deprecated', { version: '2.1.308', deprecated: 'Use 2.1.309' }, 'Claude Code 2.1.308 on the stable channel is deprecated.'],
  ])('fails when the Claude Code stable channel is %s', async (_, response, message) => {
    await expect(buildVersions({ request: fixture({ 'https://registry.npmjs.org/@anthropic-ai/claude-code/stable': response }) })).rejects.toThrow(message)
  })

  it('builds the newest qualified OpenCode release when latest leaves its series', async () => {
    const notices = []
    const versions = await buildVersions({
      notice: message => notices.push(message),
      request: fixture({
        'https://registry.npmjs.org/opencode-ai/latest': { version: '1.20.0' },
        'https://registry.npmjs.org/opencode-ai': {
          'dist-tags': { latest: '1.20.0' },
          'versions': {
            '1.19.4': {},
            '1.19.5-beta.1': {},
            '1.19.6': { deprecated: 'Broken release' },
            '1.20.0': {},
            '1.190.0': {},
            '0.0.0-dev-202610221304': {},
          },
        },
      }),
    })
    expect(versions.opencode).toBe('1.19.4')
    expect(notices).toEqual(['OpenCode 1.20.0 on the latest channel is outside the qualified 1.19 series; building 1.19.4. Qualify the adapter before widening the series.'])
  })

  it('fails instead of guessing when the qualified series has no stable release', async () => {
    await expect(buildVersions({
      request: fixture({
        'https://registry.npmjs.org/@anthropic-ai/claude-code/stable': { version: '3.0.0' },
        'https://registry.npmjs.org/@anthropic-ai/claude-code': { versions: { '3.0.0': {}, '2.2.0-rc.1': {} } },
      }),
    })).rejects.toThrow('Claude Code 3.0.0 is outside the qualified 2 series, which has no stable release.')
  })
})

describe('build-versions command', () => {
  const preload = `globalThis.fetch = async (url) => Response.json(${JSON.stringify(registry)}[url]);`
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
      expect(JSON.parse(result.stdout)).toEqual(resolved)
      expect(readFileSync(output, 'utf8')).toBe('existing=value\ncodex=0.159.0\ngh=2.101.0\nclaude=2.1.308\nopencode=1.19.4\n')
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
