import { readFileSync } from 'node:fs'
import { createServer } from 'node:http'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { parse } from 'yaml'
import {
  buildVersions,
  currentImage,
  deployUpdate,
  newer,
  toolkitUpdate,
} from '../scripts/cli-updates.mjs'

describe('cLI update deployment and rollback', () => {
  let server
  let config
  let plan
  let activeRuns
  let selectedImage
  let failCandidate
  let releases
  let restarts
  let hideImage
  const previous = `ghcr.io/owner/cairn@sha256:${'a'.repeat(64)}`
  const candidate = `ghcr.io/owner/cairn@sha256:${'b'.repeat(64)}`
  beforeEach(async () => {
    activeRuns = 0
    selectedImage = previous
    failCandidate = false
    releases = 0
    restarts = 0
    hideImage = false
    plan = {
      image: previous,
      commit: 'same-application',
      previousRuntimeId: 'old-runtime',
      versions: { codex: '0.154.0', gh: '2.100.0' },
    }
    server = createServer(async (request, response) => {
      let body = ''
      for await (const chunk of request)
        body += chunk
      const input = body ? JSON.parse(body) : null
      response.setHeader('content-type', 'application/json')
      if (request.url === '/internal/deployment-lease') {
        if (request.method === 'DELETE')
          releases++
        response.end(JSON.stringify({ activeRuns }))
      }
      else if (request.url.endsWith('/envs')) {
        if (request.method === 'PATCH')
          selectedImage = input.value
        response.end(JSON.stringify([{ key: 'CAIRN_IMAGE', ...(!hideImage && { value: selectedImage }) }]))
      }
      else if (request.url.endsWith('/restart')) {
        restarts++
        response.end('{}')
      }
      else if (request.url === '/api/v1/services/service') {
        response.end(JSON.stringify({ docker_compose_raw: readFileSync(new URL('../compose.yaml', import.meta.url), 'utf8') }))
      }
      else if (request.url === '/health') {
        response.end(JSON.stringify({ status: 'ok', commit: plan.commit, runtimeId: selectedImage === candidate && !failCandidate ? 'cli-123-1' : 'old-runtime' }))
      }
      else if (request.url === '/internal/nodes/release') {
        response.end(JSON.stringify({ protocol: 2, commit: plan.commit, image: selectedImage }))
      }
      else {
        response.writeHead(404).end('{}')
      }
    })
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
    const origin = `http://127.0.0.1:${server.address().port}`
    config = {
      coolifyUrl: origin,
      publicUrl: origin,
      serviceUuid: 'service',
      repository: 'owner/cairn',
      token: 'test-token',
      maintenanceToken: 'test-maintenance',
      runtimeId: 'cli-123-1',
    }
  })
  afterEach(async () => {
    server.closeAllConnections()
    await new Promise(resolve => server.close(resolve))
  })
  it('chooses newer stable versions and never downgrades or accepts prereleases', () => {
    expect(newer('0.99.0', '0.154.0')).toBe('0.154.0')
    expect(newer('2.100.0', '2.99.0')).toBe('2.100.0')
    expect(newer('2.100.0', '2.100.0')).toBe('2.100.0')
    for (const version of ['next', '1.0.0-beta', undefined, '1.0.0\nARG BAD'])
      expect(() => newer('1.0.0', version)).toThrow(/stable/)
  })
  it('verifies the new runtime despite an unchanged application commit', async () => {
    expect(await deployUpdate(config, plan, candidate, { intervalMs: 0, timeoutMs: 1000 })).toMatchObject({ deployed: true, runtimeId: 'cli-123-1' })
    expect(selectedImage).toBe(candidate)
    expect(releases).toBe(1)
  })
  it('explains the missing Coolify permission without exposing environment values', async () => {
    hideImage = true
    await expect(currentImage(config)).rejects.toThrow('read:sensitive permission')
    expect(restarts).toBe(0)
  })
  it('updates busy workers using restart recovery and releases the lease afterwards', async () => {
    activeRuns = 1
    expect(await deployUpdate(config, plan, candidate, { intervalMs: 0, timeoutMs: 1000 })).toMatchObject({ deployed: true })
    expect(restarts).toBe(1)
    expect(selectedImage).toBe(candidate)
    expect(releases).toBe(1)
  })
  it('does not overwrite an application release that happened during the build', async () => {
    selectedImage = `ghcr.io/owner/cairn@sha256:${'c'.repeat(64)}`
    expect(await deployUpdate(config, plan, candidate)).toMatchObject({ deployed: false })
    expect(restarts).toBe(0)
    expect(releases).toBe(0)
  })
  it('restores and verifies the previous image when the candidate remains unhealthy or stale', async () => {
    activeRuns = 1
    failCandidate = true
    await expect(deployUpdate(config, plan, candidate, { intervalMs: 1, timeoutMs: 50 })).rejects.toThrow('previous image was restored')
    expect(selectedImage).toBe(previous)
    expect(restarts).toBe(2)
    expect(releases).toBe(1)
  })
})

describe('application image tool versions', () => {
  const releases = {
    codex: { version: '0.159.0' },
    gh: { tag_name: 'v2.101.0', draft: false, prerelease: false },
  }
  const request = async url => url.includes('registry.npmjs.org') ? releases.codex : releases.gh

  it('resolves the latest stable tools without Dockerfile version defaults', async () => {
    expect(await buildVersions({ request })).toEqual({ codex: '0.159.0', gh: '2.101.0' })
    const dockerfile = readFileSync(new URL('../Dockerfile', import.meta.url), 'utf8')
    expect(dockerfile).toMatch(/^ARG CODEX_VERSION$/m)
    expect(dockerfile).toMatch(/^ARG GH_VERSION$/m)
  })

  it('passes the resolved exact versions into the CI image that is smoke-tested', () => {
    const jobs = parse(readFileSync(new URL('../.github/workflows/ci.yaml', import.meta.url), 'utf8')).jobs
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

describe('global toolkit update selection', () => {
  const now = 1800000000000
  const installedToolkit = { mise: '2026.9.4', tools: { node: '24.21.0', pnpm: '12.4.0' }, builtAt: now }
  const available = { mise: '2026.9.4', tools: { node: '24.21.0', pnpm: '12.4.0' } }
  it('skips unchanged tools and refreshes OS packages weekly', () => {
    expect(toolkitUpdate({ changed: false, installedToolkit }, available, now).changed).toBe(false)
    expect(toolkitUpdate({ changed: false, installedToolkit }, available, now + 7 * 86400000).changed).toBe(true)
  })
  it('detects mise or tool releases while preserving newer installed versions', () => {
    const result = toolkitUpdate({ changed: false, installedToolkit }, { mise: '2026.9.5', tools: { node: '24.22.0', pnpm: '12.3.0' } }, now)
    expect(result).toEqual({ changed: true, toolkit: { mise: '2026.9.5', tools: { node: '24.22.0', pnpm: '12.4.0' } } })
  })
  it('rejects unexpected tool sets and prereleases', () => {
    expect(() => toolkitUpdate({ installedToolkit }, { ...available, tools: {} }, now)).toThrow(/catalogue/)
    expect(() => toolkitUpdate({ installedToolkit }, { ...available, mise: '2026.9.5-beta' }, now)).toThrow(/stable/)
  })
})

describe('standalone tool workflow', () => {
  it('only explains the supported update path without reading production configuration or running updates', () => {
    const source = readFileSync('.github/workflows/cli-updates.yaml', 'utf8')
    const workflow = parse(source)
    expect(source).not.toMatch(/COOLIFY_|CAIRN_PUBLIC_URL|CAIRN_IMAGE|secrets\.|\$\{\{/)
    expect(Object.values(workflow.permissions).every(permission => permission === 'read')).toBe(true)
    for (const job of Object.values(workflow.jobs)) {
      expect(job.environment).toBeUndefined()
      expect(job.services).toBeUndefined()
      expect(job.steps.length).toBeGreaterThan(0)
      for (const step of job.steps) {
        expect(step.uses).toBeUndefined()
        expect(step.env).toBeUndefined()
        expect(step.run).toMatch(/^echo '[^']+'$/)
      }
    }
  })
})
