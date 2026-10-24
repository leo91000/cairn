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

  it('shares the oldest OpenCode release its adapter supports', () => {
    const adapter = readFileSync(new URL('../../crates/installation/src/opencode/mod.rs', import.meta.url), 'utf8')
    expect(adapter).toContain(`pub const MINIMUM: &str = "${engines.opencode.minimum}";`)
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

  it('qualifies the native engines that CI resolved, and a rollback to the served release', () => {
    const jobs = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8')).jobs
    const expression = value => `\${{ ${value} }}`
    const served = jobs.resolve.steps.find(step => step.id === 'served')
    expect(served.run).toBe('node scripts/served-tools.mjs')
    expect(served.env.CAIRN_BEACON_ORIGIN).toBe(expression('vars.CAIRN_BEACON_ORIGIN'))
    // Evidence records the served releases the rollback checks used, or null when
    // production was unknown, which prevents its reuse.
    const record = jobs.publish.steps.find(step => step.name === 'Record the validated image')
    expect(record.run).toContain(`served: process.env.SERVED_FOUND === 'true' ? { claude: process.env.SERVED_CLAUDE, opencode: process.env.SERVED_OPENCODE } : null`)
    // Reuse compares recorded rollback targets with the release served now.
    const reuse = jobs.resolve.steps.find(step => step.id === 'resolve')
    expect(jobs.resolve.steps.indexOf(served)).toBeLessThan(jobs.resolve.steps.indexOf(reuse))
    for (const output of ['found', 'claude', 'opencode']) {
      expect(jobs.resolve.outputs[`served-${output}`]).toBe(expression(`steps.served.outputs.${output}`))
      expect(record.env[`SERVED_${output.toUpperCase()}`]).toBe(expression(`needs.resolve.outputs.served-${output}`))
      expect(reuse.env[`SERVED_${output.toUpperCase()}`]).toBe(expression(`steps.served.outputs.${output}`))
    }

    for (const step of ['OpenCode', 'Claude Code']) {
      const run = jobs.backend.steps.find(candidate => candidate.name === `Qualify the resolved ${step} release against its native adapter`).run
      expect(run).toMatch(/served=\$\(node scripts\/served-tools\.mjs rollback-target (opencode|claude) /)
    }

    const opencode = jobs.backend.steps.find(step => step.name === 'Qualify the resolved OpenCode release against its native adapter')
    expect(opencode.env).toEqual({
      OPENCODE_VERSION: expression('needs.resolve.outputs.opencode'),
      SERVED_OPENCODE: expression('needs.resolve.outputs.served-opencode'),
    })
    const claude = jobs.backend.steps.find(step => step.name === 'Qualify the resolved Claude Code release against its native adapter')
    expect(claude.env).toEqual({
      CLAUDE_VERSION: expression('needs.resolve.outputs.claude'),
      SERVED_CLAUDE: expression('needs.resolve.outputs.served-claude'),
    })
    for (const test of ['native_cli_lists_its_model_catalog_without_inference', 'native_cli_executes_and_resumes_a_session_against_a_local_model'])
      expect(claude.run).toContain(test)
    for (const test of ['native_cli_lists_providers_and_models', 'pinned_native_cli_executes_and_resumes_without_global_plugins'])
      expect(opencode.run).toContain(test)
    // Images cannot be published unless both qualifications passed.
    expect(jobs.publish.needs).toContain('backend')
  })

  it('holds an engine at an exact version given as its channel', async () => {
    const channel = engines.opencode.channel
    engines.opencode.channel = '1.19.3'
    try {
      expect((await buildVersions({ request: fixture({ 'https://registry.npmjs.org/opencode-ai/1.19.3': { version: '1.19.3' } }) })).opencode).toBe('1.19.3')
    }
    finally {
      engines.opencode.channel = channel
    }
  })

  it('deploys a release only after its rollback checks read the served release', () => {
    const jobs = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8')).jobs
    const [guard] = jobs.deploy.steps
    expect(guard.env.SERVED_FOUND).toBe(`\${{ needs.resolve.outputs.served-found }}`)
    expect(guard.run).toContain('test "$SERVED_FOUND" != true')
    expect(guard.run).toContain('exit 1')
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
    ['missing', new Error('Request to https://registry.npmjs.org/@anthropic-ai/claude-code/stable failed: HTTP 404'), 'failed: HTTP 404'],
    ['malformed', { name: '@anthropic-ai/claude-code' }, 'Claude Code stable channel is not a stable release.'],
    ['deprecated', { version: '2.1.308', deprecated: 'Use 2.1.309' }, 'Claude Code 2.1.308 on the stable channel is deprecated.'],
  ])('fails when the Claude Code stable channel is %s', async (_, response, message) => {
    await expect(buildVersions({ request: fixture({ 'https://registry.npmjs.org/@anthropic-ai/claude-code/stable': response }) })).rejects.toThrow(message)
  })

  it('follows each engine\'s channel into new minor and major releases like Codex', async () => {
    const versions = await buildVersions({
      request: fixture({
        'https://registry.npmjs.org/@anthropic-ai/claude-code/stable': { version: '3.0.0' },
        'https://registry.npmjs.org/opencode-ai/latest': { version: '1.20.0' },
      }),
    })
    // Only channel releases are built; the package lists are never consulted.
    expect(versions).toMatchObject({ claude: '3.0.0', opencode: '1.20.0' })
  })

  it('fails when OpenCode latest moves below the oldest release its adapter supports', async () => {
    await expect(buildVersions({ request: fixture({ 'https://registry.npmjs.org/opencode-ai/latest': { version: '1.18.35' } }) }))
      .rejects
      .toThrow('OpenCode 1.18.35 on the latest channel is older than 1.19.0, the oldest release its adapter supports.')
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
      const stable = 'https://registry.npmjs.org/@anthropic-ai/claude-code/stable'
      for (const [failure, message] of [
        ['new Response("unavailable", { status: 503 })', `Request to ${stable} failed: HTTP 503`],
        ['Promise.reject(new TypeError("fetch failed"))', `Request to ${stable} failed: fetch failed`],
        ['new Response("<html>proxy</html>")', `Request to ${stable} failed: Unexpected token '<', "<html>proxy</html>" is not valid JSON`],
      ]) {
        const result = run(['build-versions'], { GITHUB_OUTPUT: output }, `globalThis.fetch = async (url) => url === ${JSON.stringify(stable)} ? ${failure} : Response.json(${JSON.stringify(registry)}[url]);`)
        expect(result.status).toBe(1)
        expect(result.stdout).toBe('')
        // The failing source is named so an operator knows which registry to check.
        expect(result.stderr.trim()).toBe(message)
        expect(readFileSync(output, 'utf8')).toBe('existing=value\n')
      }
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
