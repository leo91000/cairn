import { Buffer } from 'node:buffer'
import { readFileSync } from 'node:fs'
import { createServer } from 'node:http'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { parse, stringify } from 'yaml'
import { deploy } from '../scripts/deploy-coolify.mjs'
import { firecrackerRunnerCompose, nativeRunnerCompose, persistentRunnerCompose } from '../scripts/runner-compose.mjs'

// eslint-disable-next-line no-template-curly-in-string -- Compose resolves this expression at deployment time.
const nodeImage = '${LEO_IMAGE:-}'

describe('coolify deployment over HTTP', () => {
  let server
  let config
  let requests
  let patchStatus
  let healthResponses
  let compose
  let persistCompose
  let normalizeCompose
  let releaseImage

  beforeEach(async () => {
    requests = []
    patchStatus = 201
    compose = readFileSync(new URL('../compose.yaml', import.meta.url), 'utf8')
    persistCompose = true
    normalizeCompose = false
    releaseImage = undefined
    healthResponses = [{ status: 'ok', commit: 'new-commit' }]
    server = createServer(async (request, response) => {
      let body = ''
      for await (const chunk of request)
        body += chunk
      requests.push({
        method: request.method,
        path: request.url,
        authorization: request.headers.authorization,
        body: body ? JSON.parse(body) : undefined,
      })
      response.setHeader('content-type', 'application/json')
      if (request.url === '/api/v1/services/leo-service') {
        if (request.method === 'PATCH' && patchStatus < 300 && persistCompose) {
          compose = Buffer.from(JSON.parse(body).docker_compose_raw, 'base64').toString()
          if (normalizeCompose)
            compose = compose.replace('entrypoint: [/usr/local/bin/leo, runner-broker]', 'entrypoint:\n      - /usr/local/bin/leo\n      - runner-broker')
        }

        response.statusCode = request.method === 'PATCH' ? patchStatus : 200
        response.end(JSON.stringify({ docker_compose_raw: compose }))
        return
      }

      if (request.url === '/health') {
        const health = healthResponses.length > 1 ? healthResponses.shift() : healthResponses[0]
        response.statusCode = health ? 200 : 503
        response.end(JSON.stringify(health))
        return
      }

      if (request.url === '/internal/nodes/release') {
        response.end(JSON.stringify({ protocol: 2, commit: config.commit, image: releaseImage ?? config.image }))
        return
      }

      response.statusCode = request.method === 'PATCH' ? patchStatus : 200
      response.end('{}')
    })
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
    const origin = `http://127.0.0.1:${server.address().port}`
    config = {
      coolifyUrl: origin,
      publicUrl: origin,
      serviceUuid: 'leo-service',
      token: 'test-token',
      image: `ghcr.io/owner/leo@sha256:${'a'.repeat(64)}`,
      commit: 'new-commit',
    }
  })

  afterEach(async () => {
    server.closeAllConnections()
    await new Promise(resolve => server.close(resolve))
  })

  it('pins the image, restarts and waits through stale health and proxy errors', async () => {
    healthResponses = [{ status: 'ok', commit: 'old-commit' }, null, { status: 'ok', commit: config.commit }]
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(requests.filter(request => request.method !== 'GET')).toEqual([
      {
        method: 'PATCH',
        path: '/api/v1/services/leo-service/envs',
        authorization: 'Bearer test-token',
        body: { key: 'LEO_IMAGE', value: config.image, is_literal: true },
      },
      {
        method: 'POST',
        path: '/api/v1/services/leo-service/restart',
        authorization: 'Bearer test-token',
        body: undefined,
      },
    ])
    const healthRequests = requests.filter(request => request.path === '/health')
    expect(healthRequests).toHaveLength(3)
    expect(healthRequests.every(request => !request.authorization)).toBe(true)
  })

  it('does not restart when the image update fails', async () => {
    patchStatus = 401
    await expect(deploy(config)).rejects.toThrow('HTTP 401')
    expect(requests).toHaveLength(2)
    expect(requests.some(request => request.method === 'POST')).toBe(false)
  })

  it('preserves configured VM capacity and resource budgets when releasing', async () => {
    const document = parse(compose)
    document.services.manager.environment.CONCURRENCY = 8
    Object.assign(document.services.runner, { mem_limit: '36g', cpus: 6, pids_limit: 512 })
    document.services.runner.environment.CONCURRENCY = 8
    compose = stringify(document)
    const original = compose
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(compose).toBe(original)
    expect(requests.some(request => request.path.endsWith('/restart'))).toBe(true)
  })

  it('assigns VM resource defaults when migrating a legacy container runner', () => {
    const document = parse(compose)
    delete document.services.runner.devices
    Object.assign(document.services.runner, { mem_limit: '4g', cpus: 2, pids_limit: 128 })
    const result = parse(firecrackerRunnerCompose(stringify(document)))
    expect(result.services.runner).toMatchObject({ mem_limit: '20g', cpus: 8 })
    expect(result.services.runner.pids_limit).toBeUndefined()
    expect(result.services.runner.devices).toContain('/dev/fuse:/dev/fuse')
  })

  it.each(['mapping', 'list'])('preserves the operator pool setting across deployment (%s)', async (shape) => {
    const document = parse(compose)
    const environment = { ...document.services.runner.environment, LEO_READY_VM_POOL: 'false' }
    document.services.runner.environment = shape === 'list'
      ? Object.entries(environment).map(([key, value]) => `${key}=${value}`)
      : environment
    compose = stringify(document)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(parse(compose).services.runner.environment.LEO_READY_VM_POOL).toBe('false')
    expect(firecrackerRunnerCompose(compose)).toBe(compose)
  })

  it.each(['mapping', 'list'])('preserves explicitly configured ublk transport and device classes (%s)', async (shape) => {
    const document = parse(compose)
    const runner = document.services.runner
    const environment = { ...runner.environment, LEO_BLOCK_TRANSPORT: 'ublk' }
    runner.environment = shape === 'list'
      ? Object.entries(environment).map(([key, value]) => `${key}=${value}`)
      : environment
    runner.devices.push('/dev/ublk-control:/dev/ublk-control')
    runner.device_cgroup_rules = ['c 238:* rwm', 'b 259:* rwm']
    compose = stringify(document)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    const result = parse(compose).services.runner
    expect(result.environment.LEO_BLOCK_TRANSPORT).toBe('ublk')
    expect(result.devices).toContain('/dev/ublk-control:/dev/ublk-control')
    expect(result.device_cgroup_rules).toEqual(runner.device_cgroup_rules)
    expect(result.cap_add).toContain('SYS_RESOURCE')
    expect(result.privileged).toBeUndefined()
    expect(firecrackerRunnerCompose(compose)).toBe(compose)
  })

  it('does not grant the ublk flusher capability to a vhost-user runner', async () => {
    const document = parse(compose)
    document.services.runner.cap_add.push('SYS_RESOURCE')
    document.services.runner.environment.LEO_BLOCK_TRANSPORT = 'vhost-user'
    compose = stringify(document)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(parse(compose).services.runner.cap_add).not.toContain('SYS_RESOURCE')
    expect(firecrackerRunnerCompose(compose)).toBe(compose)
  })

  it.each(['mapping', 'list'])('preserves snapshot activation through repeated deployment (%s)', async (shape) => {
    const document = parse(compose)
    const runner = document.services.runner
    const environment = {
      ...runner.environment,
      LEO_BLOCK_TRANSPORT: 'ublk',
      LEO_DISK_LAYOUT: 'paired-ext4-v1',
      LEO_VM_SNAPSHOTS: 'true',
    }
    runner.environment = shape === 'list'
      ? Object.entries(environment).map(([key, value]) => `${key}=${value}`)
      : environment
    runner.devices.push('/dev/ublk-control:/dev/ublk-control')
    runner.device_cgroup_rules = ['c 238:* rwm', 'b 259:* rwm']
    compose = stringify(document)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(parse(compose).services.runner.environment).toMatchObject({ LEO_BLOCK_TRANSPORT: 'ublk', LEO_DISK_LAYOUT: 'paired-ext4-v1', LEO_VM_SNAPSHOTS: 'true' })
    expect(firecrackerRunnerCompose(compose)).toBe(compose)
    // Disabling new clones must retain the ability to boot existing paired disks.
    const disabled = parse(compose)
    disabled.services.runner.environment.LEO_VM_SNAPSHOTS = 'false'
    compose = stringify(disabled)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(parse(compose).services.runner.environment).toMatchObject({ LEO_DISK_LAYOUT: 'paired-ext4-v1', LEO_VM_SNAPSHOTS: 'false' })
  })

  it.each([
    { LEO_VM_SNAPSHOTS: 'true' },
    { LEO_DISK_LAYOUT: 'paired-ext4-v1' },
    { LEO_VM_SNAPSHOTS: 'yes' },
    { LEO_DISK_LAYOUT: 'unknown' },
    { LEO_VM_SNAPSHOTS: 'true', LEO_BLOCK_TRANSPORT: 'ublk', LEO_DISK_LAYOUT: 'flat-ext4-v1' },
  ])('rejects incompatible snapshot configuration before mutating the service (%j)', async (settings) => {
    const document = parse(compose)
    Object.assign(document.services.runner.environment, settings)
    compose = stringify(document)
    await expect(deploy(config, { intervalMs: 0, timeoutMs: 1000 })).rejects.toThrow()
    expect(requests.some(request => request.method === 'PATCH' || request.method === 'POST')).toBe(false)
  })

  it('rejects incomplete ublk permissions before changing or restarting the service', async () => {
    const document = parse(compose)
    document.services.runner.environment.LEO_BLOCK_TRANSPORT = 'ublk'
    compose = stringify(document)
    await expect(deploy(config, { intervalMs: 0, timeoutMs: 1000 })).rejects.toThrow('ublk')
    expect(requests.some(request => request.method === 'PATCH' || request.method === 'POST')).toBe(false)
  })

  it('adds FUSE to an existing VM runner before deploying the S3-backed image', async () => {
    const document = parse(compose)
    document.services.runner.devices = document.services.runner.devices.filter(device => !device.startsWith('/dev/fuse:'))
    compose = stringify(document)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    const patched = requests.find(request => request.method === 'PATCH' && request.path === '/api/v1/services/leo-service')
    expect(patched).toBeDefined()
    expect(parse(compose).services.runner.devices).toContain('/dev/fuse:/dev/fuse')
    expect(requests.some(request => request.path.endsWith('/restart'))).toBe(true)
  })

  it.each(['mapping', 'list'])('enables remote node updates in a legacy %s manager environment', async (shape) => {
    const document = parse(compose)
    delete document.services.manager.environment.LEO_NODE_IMAGE
    if (shape === 'list')
      document.services.manager.environment = Object.entries(document.services.manager.environment).map(([key, value]) => `${key}=${value}`)
    compose = stringify(document)
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    const environment = parse(compose).services.manager.environment
    expect(shape === 'list' ? environment : [`LEO_NODE_IMAGE=${environment.LEO_NODE_IMAGE}`]).toContain(`LEO_NODE_IMAGE=${nodeImage}`)
  })

  it('fails if a healthy service keeps serving the previous commit', async () => {
    healthResponses = [{ status: 'ok', commit: 'old-commit' }]
    await expect(deploy(config, { intervalMs: 0, timeoutMs: 25 })).rejects.toThrow('did not serve commit')
    expect(requests.some(request => request.path === '/health')).toBe(true)
  })

  it('does not report success while nodes would receive an old image', async () => {
    releaseImage = `ghcr.io/owner/leo@sha256:${'b'.repeat(64)}`
    await expect(deploy(config, { intervalMs: 0, timeoutMs: 25 })).rejects.toThrow('node image')
    expect(requests.some(request => request.path === '/internal/nodes/release')).toBe(true)
  })

  it('adds persistent runner storage and verifies it before updating the image', async () => {
    compose = compose.replace('      - runner-state:/runner-state\n', '').replace('  runner-state:\n', '')
    const original = compose
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    const migration = requests.find(request => request.method === 'PATCH' && request.path === '/api/v1/services/leo-service')
    expect(Buffer.from(migration.body.docker_compose_raw, 'base64').toString()).toBe(firecrackerRunnerCompose(original))
    expect(compose).toContain('data:/data')
    expect(compose).toContain('      - runner-state:/runner-state')
    expect(compose).toContain('\nvolumes:\n  runner-state:')
    expect(requests.slice(0, 4).map(request => [request.method, request.path])).toEqual([
      ['GET', '/api/v1/services/leo-service'],
      ['PATCH', '/api/v1/services/leo-service'],
      ['GET', '/api/v1/services/leo-service'],
      ['PATCH', '/api/v1/services/leo-service/envs'],
    ])
  })

  it('accepts Coolify reformatting the migrated entrypoint before deploying', async () => {
    compose = compose.replace('entrypoint: [/usr/local/bin/leo, runner-broker]', 'entrypoint: [node, --import, tsx, /app/server/runner-broker.ts]')
    normalizeCompose = true
    await deploy(config, { intervalMs: 0, timeoutMs: 1000 })
    expect(firecrackerRunnerCompose(compose)).toBe(compose)
    expect(requests.some(request => request.path.endsWith('/restart'))).toBe(true)
    expect(nativeRunnerCompose(compose)).toBe(compose)
  })

  it.each([{ CONCURRENCY: '12' }, ['CONCURRENCY=12']])('shares configured manager concurrency with the VM runner (%j)', (environment) => {
    const document = parse(compose)
    document.services.manager.environment = environment
    const migrated = firecrackerRunnerCompose(stringify(document))
    const result = parse(migrated)
    expect(result.services.manager.environment).toEqual(Array.isArray(environment)
      ? [...environment, `LEO_NODE_IMAGE=${nodeImage}`]
      : { ...environment, LEO_NODE_IMAGE: nodeImage })
    expect(result.services.runner.environment.CONCURRENCY).toBe('12')
    expect(firecrackerRunnerCompose(migrated)).toBe(migrated)
  })

  it('does not deploy when the mount migration was not persisted', async () => {
    compose = compose.replace('      - runner-state:/runner-state\n', '').replace('  runner-state:\n', '')
    persistCompose = false
    await expect(deploy(config)).rejects.toThrow('did not persist')
    expect(requests.some(request => request.path.endsWith('/envs') || request.path.endsWith('/restart'))).toBe(false)
  })

  it('preserves quoted mount sources, variables and unrelated read-only mounts', () => {
    // eslint-disable-next-line no-template-curly-in-string -- Literal Compose interpolation must survive the migration.
    const input = 'services:\n  manager:\n    volumes:\n      - data:/runner-state:ro\n  runner:\n    environment:\n      - SETTING=example:/runner-state:ro\n    volumes:\n      - "${DATA_VOLUME}:/runner-state:ro" # persistent\n      - other:/other:ro\n'
    // eslint-disable-next-line no-template-curly-in-string -- These are literal Compose expressions.
    expect(persistentRunnerCompose(input)).toBe(input.replace('"${DATA_VOLUME}:/runner-state:ro"', '"${DATA_VOLUME}:/runner-state:rw"'))
  })

  it.each([
    '    entrypoint: [node, --import, tsx, /app/server/runner-broker.ts]',
    '    entrypoint:\n      - node\n      - --import\n      - tsx\n      - /app/server/runner-broker.ts',
  ])('migrates the stored runner entrypoint and preserves the manager configuration', (entrypoint) => {
    const input = `services:\n  manager:\n    command: keep-this\n  runner:\n${entrypoint}\n    environment:\n      RUNNER_MANAGER_CONTAINER: unchanged\n    volumes:\n      - state:/runner-state\n`
    const migrated = nativeRunnerCompose(input)
    expect(migrated).toContain('    entrypoint: [/usr/local/bin/leo, runner-broker]')
    expect(migrated).toContain('command: keep-this')
    expect(migrated).toContain('RUNNER_MANAGER_CONTAINER: unchanged')
    expect(migrated).not.toContain('tsx')
    expect(nativeRunnerCompose(migrated)).toBe(migrated)
  })

  it.each([
    '    entrypoint: ["/usr/local/bin/leo", "runner-broker"]',
    '    entrypoint:\n      - \'/usr/local/bin/leo\'\n      - \'runner-broker\'',
    '    entrypoint: \'/usr/local/bin/leo runner-broker\'',
  ])('preserves equivalent native entrypoints', (entrypoint) => {
    const compose = `services:\n  runner:\n${entrypoint}\n    volumes:\n      - state:/runner-state\n`
    expect(nativeRunnerCompose(compose)).toBe(compose)
  })

  it.each([undefined, 'services: {}', 'services:\n  runner:\n    environment:\n      - SETTING=example:/runner-state:ro\n', 'services:\n  runner:\n    volumes:\n      - type: volume\n        target: /runner-state\n'])('rejects unverified custom Compose layouts', (input) => {
    expect(() => persistentRunnerCompose(input)).toThrow('Cannot verify runner storage')
  })
})
