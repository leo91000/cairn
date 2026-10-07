import { Buffer } from 'node:buffer'
import { appendFileSync } from 'node:fs'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { parse } from 'yaml'
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

    if (read) {
      try {
        return await response.json()
      }
      catch {
        throw new Error(`Coolify ${method} ${path} returned invalid JSON`)
      }
    }

    await response.body?.cancel()
  }

  const started = Date.now()
  const service = await api(servicePath, 'GET', undefined, true)
  const official = !!config.installationImage
  if (official) {
    let document
    try {
      document = parse(service.docker_compose_raw)
    }
    catch {
      throw new Error('Coolify returned invalid official production Compose')
    }

    const officialService = document?.services?.official
    // Reject the old manager target before changing any environment values.
    // eslint-disable-next-line no-template-curly-in-string -- Coolify must retain these Compose expressions.
    if (!officialService || document.services.manager || document.services.runner || officialService.image !== '${LEO_OFFICIAL_IMAGE:?Set the validated official image digest}'
      || Number(officialService.deploy?.replicas) !== 1 || officialService.deploy?.update_config?.order !== 'stop-first'
      // eslint-disable-next-line no-template-curly-in-string -- This must be operator-configured, not baked into Compose.
      || officialService.environment?.LEO_INSTALLATION_IMAGE !== '${LEO_INSTALLATION_IMAGE:?Set the paired installation digest}') {
      throw new Error('Coolify must target the single-process official production Compose, not the old manager')
    }
  }

  const compose = official ? service.docker_compose_raw : firecrackerRunnerCompose(service.docker_compose_raw)
  if (compose !== service.docker_compose_raw) {
    await api(servicePath, 'PATCH', { docker_compose_raw: Buffer.from(compose).toString('base64') })
    const updatedService = await api(servicePath, 'GET', undefined, true)
    if (firecrackerRunnerCompose(updatedService.docker_compose_raw) !== updatedService.docker_compose_raw)
      throw new Error('Coolify did not persist the runner configuration.')
  }

  if (official) {
    const data = [
      { key: 'LEO_OFFICIAL_IMAGE', value: image, is_literal: true },
      { key: 'LEO_INSTALLATION_IMAGE', value: config.installationImage, is_literal: true },
    ]
    const previousEnvironment = await api(`${servicePath}/envs`, 'GET', undefined, true)
    const previous = data.map(({ key }) => {
      const entry = previousEnvironment.find(env => env.key === key)
      if (!entry || typeof entry.value !== 'string' || !/^ghcr\.io\/[a-z0-9_.\-/]+@sha256:[a-f0-9]{64}$/.test(entry.value))
        throw new Error('Coolify must expose both previous image values (read:sensitive permission); refusing update')
      return { key, value: entry.value, is_literal: entry.is_literal === true }
    })

    async function persistPair(pair) {
      await api(`${servicePath}/envs/bulk`, 'PATCH', { data: pair })
      const environment = await api(`${servicePath}/envs`, 'GET', undefined, true)
      const persisted = pair.every(expected => environment.some(actual => actual.key === expected.key
        && actual.value === expected.value && actual.is_literal === expected.is_literal))
      if (!persisted)
        throw new Error('Coolify did not persist the paired image environment; refusing restart')
    }

    try {
      await persistPair(data)
    }
    catch {
      // A bulk request can fail after persisting one field, or lose its response.
      // Repair the candidate pair before any restart, even when rerunning a deploy.
      try {
        await persistPair(data)
      }
      catch (error) {
        try {
          await persistPair(previous)
        }
        catch {
          throw new Error(`Coolify image pair could not be repaired or restored; freeze restarts and repair both values manually: ${error.message}`)
        }

        throw new Error(`Previous image pair restored; no restart: ${error.message}`)
      }
    }
  }
  else {
    await api(`${servicePath}/envs`, 'PATCH', {
      key: 'LEO_IMAGE',
      value: image,
      is_literal: true,
    })
  }

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
          const releaseResponse = await fetch(new URL(official ? '/install/release' : '/internal/nodes/release', publicUrl), {
            cache: 'no-store',
            redirect: 'error',
            signal: AbortSignal.timeout(10000),
          })
          if (releaseResponse.ok) {
            const release = await releaseResponse.json()
            if (official ? release.image === config.installationImage : release.protocol === 2 && release.commit === commit && release.image === image) {
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

  throw new Error(`Deployment did not serve commit ${commit} with ${official ? 'installation' : 'node'} image ${config.installationImage || image} before the timeout`)
}

function configuration() {
  const names = ['COOLIFY_URL', 'COOLIFY_SERVICE_UUID', 'COOLIFY_TOKEN', 'LEO_OFFICIAL_ORIGIN', 'DEPLOY_IMAGE', 'DEPLOY_INSTALLATION_IMAGE', 'DEPLOY_COMMIT']
  for (const name of names) {
    if (!process.env[name])
      throw new Error(`Missing ${name}`)
  }

  for (const name of ['COOLIFY_URL', 'LEO_OFFICIAL_ORIGIN']) {
    const url = new URL(process.env[name])
    if (url.protocol !== 'https:' || url.username || url.password || url.pathname !== '/' || url.search || url.hash)
      throw new Error(`${name} must be an HTTPS origin`)
  }

  for (const name of ['DEPLOY_IMAGE', 'DEPLOY_INSTALLATION_IMAGE']) {
    if (!/^ghcr\.io\/[a-z0-9_.\-/]+@sha256:[a-f0-9]{64}$/.test(process.env[name]))
      throw new Error(`${name} must be an immutable GHCR digest`)
  }

  if (!/^[a-f0-9]{40}$/.test(process.env.DEPLOY_COMMIT))
    throw new Error('DEPLOY_COMMIT must be a full Git commit SHA')
  return {
    coolifyUrl: process.env.COOLIFY_URL,
    serviceUuid: process.env.COOLIFY_SERVICE_UUID,
    token: process.env.COOLIFY_TOKEN,
    image: process.env.DEPLOY_IMAGE,
    commit: process.env.DEPLOY_COMMIT,
    runtimeId: process.env.DEPLOY_COMMIT,
    installationImage: process.env.DEPLOY_INSTALLATION_IMAGE,
    publicUrl: process.env.LEO_OFFICIAL_ORIGIN,
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
