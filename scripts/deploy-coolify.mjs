import { Buffer } from 'node:buffer'
import { appendFileSync } from 'node:fs'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { firecrackerRunnerCompose } from './runner-compose.mjs'

export async function deploy(config, { timeoutMs = 600000, intervalMs = 2000 } = {}) {
  const {
    coolifyUrl,
    serviceUuid,
    token,
    image,
    commit,
    publicUrl,
  } = config
  const servicePath = `/api/v1/services/${encodeURIComponent(serviceUuid)}`

  async function api(path, method, body, read = false) {
    const response = await fetch(new URL(path, coolifyUrl), {
      method,
      headers: { 'authorization': `Bearer ${token}`, 'content-type': 'application/json' },
      body: body ? JSON.stringify(body) : undefined,
      redirect: 'error',
      signal: AbortSignal.timeout(15000),
    })
    // API responses can contain environment values: never log their bodies.
    if (!response.ok) {
      await response.body?.cancel()
      throw new Error(`Coolify ${method} ${path} failed (HTTP ${response.status})`)
    }

    if (read)
      return response.json()
    await response.body?.cancel()
  }

  const started = Date.now()
  const service = await api(servicePath, 'GET', undefined, true)
  const compose = firecrackerRunnerCompose(service.docker_compose_raw)
  if (compose !== service.docker_compose_raw) {
    await api(servicePath, 'PATCH', { docker_compose_raw: Buffer.from(compose).toString('base64') })
    const updatedService = await api(servicePath, 'GET', undefined, true)
    if (firecrackerRunnerCompose(updatedService.docker_compose_raw) !== updatedService.docker_compose_raw)
      throw new Error('Coolify did not persist the runner configuration.')
  }

  await api(`${servicePath}/envs`, 'PATCH', {
    key: 'LEO_IMAGE',
    value: image,
    is_literal: true,
  })
  const updated = Date.now()
  await api(`${servicePath}/restart`, 'POST')
  const restarted = Date.now()
  let polls = 0

  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    try {
      polls++
      const response = await fetch(new URL('/health', publicUrl), {
        cache: 'no-store',
        redirect: 'error',
        signal: AbortSignal.timeout(10000),
      })
      if (response.ok) {
        const health = await response.json()
        if (health.status === 'ok' && health.commit === commit && (!config.runtimeId || health.runtimeId === config.runtimeId)) {
          const releaseResponse = await fetch(new URL('/internal/nodes/release', publicUrl), {
            cache: 'no-store',
            redirect: 'error',
            signal: AbortSignal.timeout(10000),
          })
          if (releaseResponse.ok) {
            const release = await releaseResponse.json()
            if (release.protocol === 2 && release.commit === commit && release.image === image) {
              return {
                updateMs: updated - started,
                restartMs: restarted - updated,
                healthyMs: Date.now() - restarted,
                totalMs: Date.now() - started,
                polls,
              }
            }
          }
          else {
            await releaseResponse.body?.cancel()
          }
        }
      }
      else {
        await response.body?.cancel()
      }
    }
    catch {
      // The reverse proxy can briefly return errors during replacement.
    }

    await setTimeout(intervalMs)
  }

  throw new Error(`Deployment did not serve commit ${commit} with node image ${image} before the timeout`)
}

function configuration() {
  const names = ['COOLIFY_URL', 'COOLIFY_SERVICE_UUID', 'COOLIFY_TOKEN', 'LEO_PUBLIC_URL', 'DEPLOY_IMAGE', 'DEPLOY_COMMIT']
  for (const name of names) {
    if (!process.env[name])
      throw new Error(`Missing ${name}`)
  }

  for (const name of ['COOLIFY_URL', 'LEO_PUBLIC_URL']) {
    const url = new URL(process.env[name])
    if (url.protocol !== 'https:' || url.username || url.password || url.pathname !== '/' || url.search || url.hash)
      throw new Error(`${name} must be an HTTPS origin`)
  }

  if (!/^ghcr\.io\/[a-z0-9_.\-/]+@sha256:[a-f0-9]{64}$/.test(process.env.DEPLOY_IMAGE))
    throw new Error('DEPLOY_IMAGE must be an immutable GHCR digest')
  if (!/^[a-f0-9]{40}$/.test(process.env.DEPLOY_COMMIT))
    throw new Error('DEPLOY_COMMIT must be a full Git commit SHA')
  return {
    coolifyUrl: process.env.COOLIFY_URL,
    serviceUuid: process.env.COOLIFY_SERVICE_UUID,
    token: process.env.COOLIFY_TOKEN,
    image: process.env.DEPLOY_IMAGE,
    commit: process.env.DEPLOY_COMMIT,
    runtimeId: process.env.DEPLOY_COMMIT,
    publicUrl: process.env.LEO_PUBLIC_URL,
  }
}

if (import.meta.main) {
  try {
    const config = configuration()
    const timings = await deploy(config)
    const result = `Deployed ${config.image}\nVerified commit ${config.commit} at ${config.publicUrl}\nDeployment timings: ${JSON.stringify(timings)}\n`
    console.log(result)
    if (process.env.GITHUB_STEP_SUMMARY)
      appendFileSync(process.env.GITHUB_STEP_SUMMARY, result)
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
