import assert from 'node:assert/strict'
import { readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { selectJobClock } from './ci-diagnostic-source.mjs'

export function commandBudget(clock, now = Date.now()) {
  const elapsed = now - clock.startedAt
  assert(Number.isFinite(elapsed) && elapsed >= 0, 'Invalid job clock')
  // Leave five minutes for evidence and another thirty seconds for group cleanup.
  const seconds = Math.floor(Math.min(25 * 60000, 60 * 60000 - elapsed - 330000) / 1000)
  assert(seconds > 0, 'No diagnostic work budget remains')
  return seconds
}

export function validateProgress(events) {
  const methods = ['filAndConversationShowOneHeaderAndTheConnectionSheet', 'switchingFromTheConnectionSheetSelectsTheOtherInstallation']
  const expected = new Set(Array.from({ length: 100 }, (_, trial) => methods.map(method => `${method}[trial=${trial}, cutout=${trial % 2 ? 48 : 0}dp]`)).flat())
  const starts = events.filter(event => event.event === 'test-start')
  const ends = events.filter(event => event.event === 'test-end')
  const suites = events.filter(event => event.event === 'suite-end')
  const exactNames = values => values.length === expected.size && new Set(values.map(value => value.name)).size === expected.size && values.every(value => expected.has(value.name))
  const complete = exactNames(starts) && exactNames(ends) && suites.length === 1 && suites[0].tests === 200
  const successful = complete && ends.every(event => event.result === 'SUCCESS' && event.errors.length === 0) && suites[0].result === 'SUCCESS' && suites[0].failed === 0 && suites[0].skipped === 0
  return {
    qualificationClaim: false,
    complete,
    successful,
    started: starts.length,
    finished: ends.length,
    pending: starts.filter(start => !ends.some(end => end.name === start.name)).map(start => start.name),
    failures: ends.filter(end => end.result !== 'SUCCESS'),
  }
}

async function main() {
  const [mode, file, report] = process.argv.slice(2)
  if (mode === 'clock') {
    const { GITHUB_REPOSITORY: repository, GITHUB_RUN_ID: run, GITHUB_RUN_ATTEMPT: attempt } = process.env
    assert(/^[\w.-]+\/[\w.-]+$/.test(repository ?? '') && /^\d+$/.test(run ?? '') && /^\d+$/.test(attempt ?? ''), 'Invalid CI identity')
    const response = await fetch(`https://api.github.com/repos/${repository}/actions/runs/${run}/attempts/${attempt}/jobs?per_page=100`, {
      headers: { Authorization: `Bearer ${process.env.GITHUB_TOKEN}`, Accept: 'application/vnd.github+json' },
      signal: AbortSignal.timeout(10000),
    })
    assert(response.ok, 'Job clock acquisition failed')
    const clock = selectJobClock(await response.json(), process.env.RUNNER_NAME)
    commandBudget(clock)
    writeFileSync(file, `${JSON.stringify(clock)}\n`)
  }
  else if (mode === 'budget') {
    console.log(commandBudget(JSON.parse(readFileSync(file, 'utf8'))))
  }
  else if (mode === 'progress') {
    let result
    try {
      result = validateProgress(readFileSync(file, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)))
    }
    catch {
      result = { qualificationClaim: false, complete: false, successful: false, evidenceUnavailable: true }
    }
    writeFileSync(report, `${JSON.stringify(result, null, 2)}\n`)
    if (!result.successful)
      process.exitCode = 1
  }
  else {
    throw new Error('Unknown header observer operation')
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url))
  await main()
