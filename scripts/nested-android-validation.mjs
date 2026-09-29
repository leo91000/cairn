import { execFile } from 'node:child_process'
import { appendFile, readFile } from 'node:fs/promises'
import process from 'node:process'
import { setTimeout as sleep } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const sha = /^[a-f0-9]{40}$/
const digestPattern = /^sha256:[a-f0-9]{64}$/
const repositoryPattern = /^[\w.-]+\/[\w.-]+$/

function context(digest) {
  if (!digestPattern.test(digest || ''))
    throw new Error('An immutable image digest is required')
  return `nested-android/intel/${digest.slice(7)}`
}

function reportUrl(value) {
  const url = new URL(value)
  if (url.protocol !== 'https:' || url.username || url.password)
    throw new Error('Evidence must have a durable HTTPS report URL')
  return url.href
}

export function evidenceStatus(evidence, url) {
  if (evidence.schema !== 1 || !repositoryPattern.test(evidence.repository || '') || !sha.test(evidence.commit || '')
    || evidence.cpuVendor !== 'GenuineIntel' || evidence.api !== '34' || evidence.system !== 'aosp'
    || evidence.results?.length !== 2 || evidence.results[0]?.mode !== 'first' || evidence.results[1]?.mode !== 'resume'
    || evidence.results.some(result => result.status !== 'passed')) {
    throw new Error('Incomplete Intel Android boot, interaction and restart evidence')
  }

  const prefix = `ghcr.io/${evidence.repository.toLowerCase()}@`
  if (!evidence.image?.startsWith(prefix))
    throw new Error('Evidence image does not belong to this repository')
  return {
    state: 'success',
    context: context(evidence.image.slice(prefix.length)),
    description: 'Intel KVM: Android 34 AOSP boot, interaction and restart passed',
    target_url: reportUrl(url),
  }
}

export async function waitForValidation(config, {
  statuses,
  sleep: pause = sleep,
  now = Date.now,
  timeoutMs = 40 * 60 * 1000,
} = {}) {
  if (!repositoryPattern.test(config.repository || '') || !sha.test(config.commit || '') || !config.validator)
    throw new Error('Repository, exact commit and trusted validator are required')
  const expected = context(config.digest)
  const deadline = now() + timeoutMs
  while (now() < deadline) {
    // The API returns newest first. Never fall back to an older success.
    const status = (await statuses()).find(value => value.context === expected)
    if (status) {
      if (status.creator?.login?.toLowerCase() !== config.validator.toLowerCase())
        throw new Error('Intel qualification was not recorded by the trusted validator')
      if (['failure', 'error'].includes(status.state))
        throw new Error('Intel qualification failed')
      if (status.state === 'success') {
        reportUrl(status.target_url)
        return status
      }
    }

    await pause(15000)
  }

  throw new Error(`Timed out waiting for Intel qualification of ${config.commit} / ${config.digest}`)
}

if (import.meta.main) {
  const gh = async args => JSON.parse((await exec('gh', args, { timeout: 30000, maxBuffer: 8 * 1024 * 1024 })).stdout)
  if (process.argv[2] === 'record') {
    const evidence = JSON.parse(await readFile(process.argv[3], 'utf8'))
    const status = evidenceStatus(evidence, process.argv[4])
    const args = ['api', '--method', 'POST', `repos/${evidence.repository}/statuses/${evidence.commit}`]
    for (const [name, value] of Object.entries(status))
      args.push('-f', `${name}=${value}`)
    const saved = await gh(args)
    console.log(`Recorded Intel qualification ${saved.id}: ${status.target_url}`)
  }
  else if (process.argv[2] === 'wait') {
    const repository = process.env.GITHUB_REPOSITORY
    const commit = process.env.GITHUB_SHA
    const digest = process.env.IMAGE_DIGEST
    const validator = process.env.NESTED_KVM_VALIDATOR || repository?.split('/')[0]
    console.log(`Waiting for Intel qualification: ${commit} / ${digest}; validator ${validator}`)
    const status = await waitForValidation({
      repository,
      commit,
      digest,
      validator,
    }, {
      statuses: async () => (await gh(['api', '--paginate', '--slurp', `repos/${repository}/commits/${commit}/statuses?per_page=100`])).flat(),
    })
    if (process.env.GITHUB_OUTPUT)
      await appendFile(process.env.GITHUB_OUTPUT, `status-id=${status.id}\nreport-url=${status.target_url}\n`)
    if (process.env.GITHUB_STEP_SUMMARY)
      await appendFile(process.env.GITHUB_STEP_SUMMARY, `Intel Android qualification passed for \`${digest}\`: [evidence](${status.target_url}).\n`)
  }
  else {
    throw new Error('Usage: nested-android-validation.mjs record <evidence.json> <report-url> | wait')
  }
}
