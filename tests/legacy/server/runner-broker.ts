import type { RunnerPlan } from './execution.ts'
import { Buffer } from 'node:buffer'
import { timingSafeEqual } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import http from 'node:http'
import path from 'node:path'
import process from 'node:process'
import { dockerJson, dockerOutput, dockerRequest } from './docker.ts'
import { RunnerLifecycle } from './runner-lifecycle.ts'
import sandboxSeccomp from './runner-seccomp.json' with { type: 'json' }

const label = 'leo.agent-run'
const dataDirectory = process.env.DATA_DIR || '/data'
const manager = process.env.RUNNER_MANAGER_CONTAINER || 'leo-manager'
const containerName = (id: string) => `leo-run-${id}`

export function translateMount(source: string, mounts: { Source: string, Destination: string }[]) {
  const mount = [...mounts].sort((a, b) => b.Destination.length - a.Destination.length).find(mount => source === mount.Destination || source.startsWith(`${mount.Destination}/`))
  if (!mount || path.normalize(source) !== source || source.includes(':'))
    throw new Error('Runner mount is outside the manager volumes.')
  return path.join(mount.Source, path.relative(mount.Destination, source))
}

async function planFor(id: string): Promise<RunnerPlan> {
  const plan = JSON.parse(await readFile(path.join(dataDirectory, 'runner-plans', `${id}.json`), 'utf8')) as RunnerPlan
  if (plan.id !== id || (plan.expires !== null && (!Number.isSafeInteger(plan.expires) || plan.expires <= Date.now() || plan.expires > Date.now() + 13 * 3600000)))
    throw new Error('Run plan is invalid or expired.')
  return plan
}

async function createRun(id: string) {
  const plan = await planFor(id)
  const host = await dockerJson('GET', `/containers/${encodeURIComponent(manager)}/json`)
  // The worker always runs the same immutable image as its manager.
  const binds = plan.mounts.map(mount => `${translateMount(mount.source, host.Mounts)}:${mount.target}:${mount.readOnly ? 'ro' : 'rw'}`)
  const inputSource = path.join(dataDirectory, 'runner-plans', `${id}.json`)
  // Only the prompt file is visible; the broker's authentication and other plans stay outside.
  binds.push(`${translateMount(inputSource, host.Mounts)}:/run/leo-plan.json:ro`)
  await dockerJson('POST', `/containers/create?name=${containerName(id)}`, {
    Image: host.Image,
    User: '1000:1000',
    Entrypoint: ['/usr/local/bin/node', '--import', 'tsx', '/app/server/runner-entry.ts'],
    Cmd: [],
    Env: ['HOME=/home/node', 'CODEX_HOME=/home/node/.codex', 'NODE_ENV=production', 'PATH=/pnpm/bin:/pnpm:/usr/local/bin:/usr/bin:/bin'],
    WorkingDir: '/app',
    Labels: { [label]: 'true', 'leo.expires': `${plan.expires}` },
    Healthcheck: { Test: ['NONE'] },
    HostConfig: {
      Binds: binds,
      ReadonlyRootfs: true,
      CapDrop: ['ALL'],
      SecurityOpt: ['no-new-privileges:true', ...(plan.sandbox === 'yolo' ? [] : [`seccomp=${JSON.stringify(sandboxSeccomp)}`, ...(process.env.RUNNER_APPARMOR_PROFILE ? [`apparmor=${process.env.RUNNER_APPARMOR_PROFILE}`] : [])])],
      Init: true,
      PidsLimit: 512,
      Memory: 4 * 1024 ** 3,
      NanoCpus: 2 * 1000000000,
      NetworkMode: 'bridge',
      Tmpfs: { '/tmp': 'rw,nosuid,nodev,size=1g,mode=1777', '/run': 'rw,nosuid,nodev,size=16m' },
      LogConfig: { Type: 'json-file', Config: { 'max-size': '10m', 'max-file': '2' } },
    },
  })
  await dockerJson('POST', `/containers/${containerName(id)}/start`)
}

async function removeRun(id: string) {
  await dockerJson('DELETE', `/containers/${containerName(id)}?force=true&v=true`).catch((error) => {
    if (error.statusCode !== 404)
      throw error
  })
}

async function main() {
  const lifecycle = new RunnerLifecycle(process.env.RUNNER_STATE_DIR || '/runner-state', createRun, removeRun)
  const server = http.createServer(async (request, response) => {
    try {
      if (request.url === '/health') {
        response.end('ok')
        return
      }

      const secret = (await readFile(path.join(dataDirectory, 'runner-secret'), 'utf8')).trim()
      const supplied = request.headers.authorization?.replace(/^Bearer /, '') ?? ''
      if (secret.length !== supplied.length || !timingSafeEqual(Buffer.from(secret), Buffer.from(supplied))) {
        response.writeHead(401).end()
        return
      }

      const match = request.url?.match(/^\/runs\/([a-f0-9-]{36})(?:\/(logs|wait))?$/)
      if (!match) {
        response.writeHead(404).end()
        return
      }

      const [, id, action] = match
      if (request.method === 'POST' && !action) {
        await lifecycle.start(id)
        response.end('{}')
      }
      else if (request.method === 'DELETE' && !action) {
        await lifecycle.stop(id)
        response.end('{}')
      }
      else if (request.method === 'GET' && action === 'logs') {
        const stream = await dockerRequest('GET', `/containers/${containerName(id)}/logs?follow=1&stdout=1&stderr=1`)
        response.writeHead(200, { 'Content-Type': 'application/octet-stream' })
        const heartbeat = setInterval(() => response.write(Buffer.alloc(8)), 15000)
        try {
          for await (const frame of dockerOutput(stream)) {
            const header = Buffer.alloc(8)
            header[0] = frame.stderr ? 2 : 1
            header.writeUInt32BE(frame.data.length, 4)
            response.write(Buffer.concat([header, frame.data]))
          }

          response.end()
        }
        finally {
          clearInterval(heartbeat)
        }
      }
      else if (request.method === 'POST' && action === 'wait') {
        response.writeHead(200, { 'Content-Type': 'application/json' })
        response.flushHeaders()
        const heartbeat = setInterval(() => response.write(' '), 15000)
        try {
          const result = await dockerJson('POST', `/containers/${containerName(id)}/wait`)
          response.end(JSON.stringify(result))
        }
        finally {
          clearInterval(heartbeat)
        }
      }
      else {
        response.writeHead(405).end()
      }
    }
    catch (error) {
      if (!response.headersSent)
        response.writeHead(500, { 'Content-Type': 'application/json' })
      response.end(JSON.stringify({ error: (error as Error).message }))
    }
  })
  server.requestTimeout = 0
  server.listen(4311, '0.0.0.0')
  const cleanup = setInterval(async () => {
    try {
      const containers = await dockerJson('GET', `/containers/json?all=1&filters=${encodeURIComponent(JSON.stringify({ label: [`${label}=true`] }))}`)
      for (const container of containers) {
        if (container.Labels['leo.expires'] !== 'null' && Number(container.Labels['leo.expires']) + 30000 < Date.now())
          await dockerJson('DELETE', `/containers/${container.Id}?force=true&v=true`)
      }
    }
    catch { /* retry expired leases on the next sweep */ }
  }, 30000)
  cleanup.unref()
}

if (process.argv[1] === import.meta.filename) {
  main().catch((error) => {
    console.error(error)
    process.exitCode = 1
  })
}
