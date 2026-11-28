import { execFileSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

const backendLayouts = [
  { launcher: 'scripts/dev/test-backend.mjs', namespace: 'scripts/ci/test-native-namespace.mjs' },
  { launcher: 'scripts/test-backend.mjs', namespace: 'scripts/test-native-namespace.mjs' },
]

export function backendScriptsAt(source) {
  const paths = backendLayouts.flatMap(layout => Object.values(layout))
  const entries = execFileSync('git', ['ls-tree', '-r', '--full-tree', 'HEAD', '--', ...paths], {
    cwd: source,
    encoding: 'utf8',
    timeout: 10000,
  })
  const regular = new Set(entries.split('\n').flatMap((entry) => {
    const match = entry.match(/^100(?:644|755) blob [a-f0-9]+\t(.+)$/)
    return match ? [match[1]] : []
  }))
  const scripts = backendLayouts.find(layout => Object.values(layout).every(file => regular.has(file)))
  if (!scripts)
    throw new Error('Pinned backend source lacks a supported script layout')
  return scripts
}

export function selectJobClock(value, runner, now = Date.now()) {
  if (!Array.isArray(value.jobs) || value.total_count > 100 || !runner)
    throw new Error('Incomplete CI job clock inventory')
  const jobs = value.jobs.filter(job => job.runner_name === runner && job.status === 'in_progress')
  const startedAt = jobs.length === 1 ? Date.parse(jobs[0].started_at) : Number.NaN
  if (!Number.isFinite(startedAt) || startedAt > now || now - startedAt >= 90 * 60000)
    throw new Error('Current CI job clock is unavailable or ambiguous')
  return { jobId: jobs[0].id, startedAt }
}

async function main() {
  const role = process.argv[2]
  const candidate = process.env.DIAGNOSTIC_CANDIDATE
  const reference = process.env.DIAGNOSTIC_REFERENCE
  const immutable = value => /^[0-9a-f]{40}$/.test(value ?? '') && value !== '0'.repeat(40)
  if (!['backend', 'native'].includes(role) || process.env.BACKEND_ONLY !== 'true'
    || process.env.VALIDATION_ONLY !== 'true' || (process.env.BACKEND_QUALIFICATION ?? 'none') !== 'none'
    || !immutable(candidate) || (role === 'native' && !immutable(reference))) {
    throw new Error('Diagnostics require validation-only/backend-only, immutable source SHAs and no backend-change qualification')
  }

  const repository = process.env.GITHUB_REPOSITORY
  const run = process.env.GITHUB_RUN_ID
  const attempt = process.env.GITHUB_RUN_ATTEMPT
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository ?? '') || !/^\d+$/.test(run ?? '') || !/^\d+$/.test(attempt ?? ''))
    throw new Error('Invalid CI clock identity')
  const response = await fetch(`https://api.github.com/repos/${repository}/actions/runs/${run}/attempts/${attempt}/jobs?per_page=100`, {
    headers: { Authorization: `Bearer ${process.env.GITHUB_TOKEN}`, Accept: 'application/vnd.github+json' },
    signal: AbortSignal.timeout(10000),
  })
  if (!response.ok)
    throw new Error('CI job clock read failed')
  // GitHub's actual job start includes service/bootstrap time before the first
  // step. Never reset that budget to the observer's own startup time.
  const clock = selectJobClock(await response.json(), process.env.RUNNER_NAME)
  const output = path.resolve(`${role}-diagnostics`)
  mkdirSync(output, { mode: 0o700 })
  const revisions = role === 'native' ? { candidate, reference } : { candidate }
  const sources = {}
  for (const [name, sha] of Object.entries(revisions)) {
    execFileSync('git', ['fetch', '--no-tags', '--no-write-fetch-head', 'origin', sha], { stdio: 'inherit' })
    const source = path.join(process.env.RUNNER_TEMP, `cairn-diagnostic-${name}`)
    execFileSync('git', ['worktree', 'add', '--detach', source, sha], { stdio: 'inherit' })
    const actual = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: source, encoding: 'utf8' }).trim()
    if (actual !== sha)
      throw new Error('Diagnostic source identity mismatch')
    sources[name] = source
  }

  const backendScripts = role === 'backend' ? backendScriptsAt(sources.candidate) : undefined
  if (backendScripts)
    execFileSync(process.execPath, ['scripts/ci/backend-ci-phase-overlay.mjs', path.join(sources.candidate, backendScripts.launcher)], { stdio: 'inherit' })
  writeFileSync(path.join(output, 'source.json'), `${JSON.stringify({
    diagnosticOnly: true,
    clock,
    observer: process.env.GITHUB_SHA,
    revisions,
    sources,
    backendScripts,
    qualificationClaim: false,
  })}\n`, { mode: 0o600 })
  const backendEnv = backendScripts
    ? `CI_DIAGNOSTIC_BACKEND_LAUNCHER=${backendScripts.launcher}\nCI_DIAGNOSTIC_BACKEND_NAMESPACE=${backendScripts.namespace}\n`
    : ''
  writeFileSync(process.env.GITHUB_ENV, `CI_JOB_STARTED_AT=${clock.startedAt / 1000}\nCI_DIAGNOSTIC_SOURCE=${sources.candidate}\nCI_DIAGNOSTIC_REFERENCE_SOURCE=${sources.reference ?? ''}\n${backendEnv}`, { flag: 'a' })
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  await main()
