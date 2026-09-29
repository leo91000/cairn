import { readFileSync } from 'node:fs'
import { createServer } from 'node:http'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import {
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
  const previous = `ghcr.io/owner/leo@sha256:${'a'.repeat(64)}`
  const candidate = `ghcr.io/owner/leo@sha256:${'b'.repeat(64)}`
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
        response.end(JSON.stringify([{ key: 'LEO_IMAGE', ...(!hideImage && { value: selectedImage }) }]))
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
      repository: 'owner/leo',
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
  it('defers busy workers without restarting or leaving a lease behind', async () => {
    activeRuns = 1
    expect(await deployUpdate(config, plan, candidate)).toMatchObject({ deployed: false })
    expect(restarts).toBe(0)
    expect(selectedImage).toBe(previous)
    expect(releases).toBe(1)
  })
  it('does not overwrite an application release that happened during the build', async () => {
    selectedImage = `ghcr.io/owner/leo@sha256:${'c'.repeat(64)}`
    expect(await deployUpdate(config, plan, candidate)).toMatchObject({ deployed: false })
    expect(restarts).toBe(0)
    expect(releases).toBe(0)
  })
  it('restores and verifies the previous image when the candidate remains unhealthy or stale', async () => {
    failCandidate = true
    await expect(deployUpdate(config, plan, candidate, { intervalMs: 1, timeoutMs: 50 })).rejects.toThrow('previous image was restored')
    expect(selectedImage).toBe(previous)
    expect(restarts).toBe(2)
    expect(releases).toBe(1)
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
