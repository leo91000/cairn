import { execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { appendFile, readFile, writeFile } from 'node:fs/promises'
import process from 'node:process'
import { newerTool } from '../deploy/toolkit/versions.mjs'
import { deploy } from './deploy-coolify.mjs'

const stable = /^\d+\.\d+\.\d+$/
export function newer(current, available) {
  if (!stable.test(current) || !stable.test(available))
    throw new Error('CLI versions must be stable semantic versions.')
  const left = current.split('.').map(Number)
  const right = available.split('.').map(Number)
  for (let index = 0; index < 3; index++) {
    if (left[index] !== right[index])
      return right[index] > left[index] ? available : current
  }

  return current
}

function validImage(image, repository) {
  if (!image?.startsWith(`ghcr.io/${repository}@sha256:`) || !/@sha256:[a-f0-9]{64}$/.test(image))
    throw new Error('Expected an immutable image from this repository.')
  return image
}

async function json(url, options = {}) {
  const response = await fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(15000) })
  if (!response.ok) {
    await response.body?.cancel()
    throw new Error(`Request failed (HTTP ${response.status}).`)
  }

  return response.json()
}

export async function currentImage(config) {
  const envs = await json(`${config.coolifyUrl}/api/v1/services/${encodeURIComponent(config.serviceUuid)}/envs`, { headers: { authorization: `Bearer ${config.token}` } })
  const image = envs.find(entry => entry.key === 'LEO_IMAGE')
  if (image && !Object.hasOwn(image, 'value'))
    throw new Error('Coolify hides environment values. The deployment token needs read:sensitive permission.')
  return validImage(image?.value, config.repository)
}

export async function discover(config) {
  const [image, health, codex, github] = await Promise.all([
    currentImage(config),
    json(`${config.publicUrl}/health`),
    json('https://registry.npmjs.org/@openai/codex/latest'),
    json('https://api.github.com/repos/cli/cli/releases/latest', { headers: { Accept: 'application/vnd.github+json', ...(config.githubToken ? { authorization: `Bearer ${config.githubToken}` } : {}) } }),
  ])
  if (health.status !== 'ok' || !/^[a-f0-9]{40}$/.test(health.commit) || !health.runtimeId)
    throw new Error('The deployed application does not report its runtime identity. Deploy the CLI updater release first.')
  if (github.draft || github.prerelease)
    throw new Error('GitHub CLI latest release is not stable.')
  const versions = { codex: newer(health.tools?.codex, codex.version), gh: newer(health.tools?.gh, github.tag_name?.replace(/^v/, '')) }
  return {
    installedToolkit: health.toolkit || null,
    image,
    baseImage: validImage(health.baseImage || image, config.repository),
    commit: health.commit,
    previousRuntimeId: health.runtimeId,
    versions,
    changed: versions.codex !== health.tools.codex || versions.gh !== health.tools.gh,
  }
}

export async function deployUpdate(config, plan, image, options = { timeoutMs: 300000 }) {
  validImage(image, config.repository)
  const owner = randomUUID()
  const lease = method => json(`${config.publicUrl}/internal/deployment-lease`, { method, headers: { 'authorization': `Bearer ${config.maintenanceToken}`, 'content-type': 'application/json' }, body: JSON.stringify({ owner }) })
  if (await currentImage(config) !== plan.image)
    return { deployed: false, reason: 'Deployment changed while this update was being tested. Retry on the next check.' }
  const paused = await lease('POST')
  try {
    if (paused.activeRuns !== 0)
      return { deployed: false, reason: 'An agent is running. Update deferred; queued work can continue.' }
    const health = await json(`${config.publicUrl}/health`)
    if (health.commit !== plan.commit || health.runtimeId !== plan.previousRuntimeId || await currentImage(config) !== plan.image)
      return { deployed: false, reason: 'Runtime changed before deployment. Update deferred.' }
    try {
      await deploy({
        ...config,
        image,
        commit: plan.commit,
        runtimeId: config.runtimeId,
      }, options)
      return {
        deployed: true,
        image,
        runtimeId: config.runtimeId,
        versions: plan.versions,
      }
    }
    catch (error) {
      if (await currentImage(config) !== image)
        throw new Error('Update failed; image changed externally, so automatic rollback was not attempted.', { cause: error })
      await deploy({
        ...config,
        image: plan.image,
        commit: plan.commit,
        runtimeId: plan.previousRuntimeId,
      }, options)
      throw new Error('CLI update failed verification. The previous image was restored.', { cause: error })
    }
  }
  finally {
    await lease('DELETE')
  }
}

export function toolkitUpdate(plan, available, now = Date.now()) {
  const installed = plan.installedToolkit
  if (Object.keys(installed.tools).sort().join() !== Object.keys(available.tools).sort().join())
    throw new Error('Toolkit update does not match the deployed tool catalogue.')
  const toolkit = { mise: newer(installed.mise, available.mise), tools: {} }
  let changed = plan.changed || toolkit.mise !== installed.mise || now - installed.builtAt >= 7 * 86400000
  for (const [tool, version] of Object.entries(available.tools)) {
    toolkit.tools[tool] = newerTool(tool, installed.tools[tool], version)
    changed ||= toolkit.tools[tool] !== installed.tools[tool]
  }

  return { toolkit, changed }
}

function configuration() {
  for (const key of ['COOLIFY_URL', 'COOLIFY_SERVICE_UUID', 'COOLIFY_TOKEN', 'LEO_PUBLIC_URL', 'GITHUB_REPOSITORY']) {
    if (!process.env[key])
      throw new Error(`Missing ${key}`)
  }

  for (const key of ['COOLIFY_URL', 'LEO_PUBLIC_URL']) {
    const url = new URL(process.env[key])
    if (url.protocol !== 'https:' || url.username || url.password || url.pathname !== '/' || url.search || url.hash)
      throw new Error(`${key} must be an HTTPS origin.`)
  }

  return {
    coolifyUrl: process.env.COOLIFY_URL.replace(/\/$/, ''),
    serviceUuid: process.env.COOLIFY_SERVICE_UUID,
    token: process.env.COOLIFY_TOKEN,
    publicUrl: process.env.LEO_PUBLIC_URL.replace(/\/$/, ''),
    repository: process.env.GITHUB_REPOSITORY.toLowerCase(),
    githubToken: process.env.GH_TOKEN,
    maintenanceToken: process.env.LEO_MAINTENANCE_TOKEN,
    runtimeId: process.env.UPDATE_ID,
  }
}

if (import.meta.main) {
  try {
    const config = configuration()
    if (process.argv[2] === 'check') {
      const plan = await discover(config)
      if (plan.installedToolkit) {
        const available = JSON.parse(execFileSync('docker', ['run', '--rm', '--env', 'GITHUB_TOKEN', '--entrypoint', '/usr/local/bin/node', plan.image, '/opt/leo-toolkit/manage.mjs', 'resolve'], {
          env: { ...process.env, GITHUB_TOKEN: config.githubToken || '' },
          encoding: 'utf8',
          timeout: 240000,
          maxBuffer: 1024 * 1024,
        }))
        Object.assign(plan, toolkitUpdate(plan, available))
      }

      await writeFile('cli-update-plan.json', JSON.stringify(plan, null, 2))
      if (process.env.GITHUB_OUTPUT)
        await appendFile(process.env.GITHUB_OUTPUT, `changed=${plan.changed}\nbase_image=${plan.baseImage}\ncommit=${plan.commit}\ncodex=${plan.versions.codex}\ngh=${plan.versions.gh}\n`)
      console.log(JSON.stringify({ changed: plan.changed, versions: plan.versions, commit: plan.commit }))
    }
    else if (process.argv[2] === 'deploy') {
      if (!config.maintenanceToken || !/^cli-\d+-\d+$/.test(config.runtimeId))
        throw new Error('Missing maintenance credential or invalid update identity.')
      const result = await deployUpdate(config, JSON.parse(await readFile('cli-update-plan.json', 'utf8')), process.env.UPDATE_IMAGE)
      console.log(JSON.stringify(result))
      if (process.env.GITHUB_STEP_SUMMARY)
        await appendFile(process.env.GITHUB_STEP_SUMMARY, `CLI update: ${JSON.stringify(result)}\n`)
    }
    else {
      throw new Error('Usage: cli-updates.mjs check|deploy')
    }
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
