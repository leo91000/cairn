import { execFileSync, spawnSync } from 'node:child_process'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { isolateTestCredentials } from './test-environment.mjs'

const root = fileURLToPath(new URL('..', import.meta.url))

export const suites = {
  quality: [['pnpm', 'check']],
  contracts: [
    ['pnpm', 'check:contracts'],
    ['apps/android/gradlew', '-p', 'scripts/contracts-kotlin', 'spotlessCheck', 'test', '--no-daemon'],
    ['git', 'diff', '--exit-code', '--', 'crates/installation/schemas/'],
  ],
  rust: [['cargo', 'clippy', '--locked', '--workspace', '--all-targets', '--', '-D', 'warnings']],
  backend: [['pnpm', 'test:backend'], ['pnpm', 'test:backend', '--doc']],
  android: [['apps/android/gradlew', '-p', 'apps/android', 'spotlessCheck', 'testDebugUnitTest', 'lintDebug', '--no-daemon']],
  browser: [
    ['cargo', 'build', '--locked', '--bin', 'cairn', '--bin', 'cairn-beacon', '--example', 'worker_fixture', '--example', 'browser_fixture'],
    ['pnpm', 'build'],
    ['pnpm', 'test:e2e'],
  ],
}

export function validationPlan(names) {
  const selected = names.length ? names : ['quality', 'contracts', 'rust', 'backend']
  const expanded = selected.flatMap(name => name === 'all' ? Object.keys(suites) : [name])
  return [...new Set(expanded)].map((name) => {
    if (!Object.hasOwn(suites, name))
      throw new Error(`Unknown validation suite: ${name}. Choose ${Object.keys(suites).join(', ')} or all.`)
    return { name, commands: suites[name] }
  })
}

export function runPlan(plan, run) {
  for (const { name, commands } of plan) {
    for (const [command, ...args] of commands) {
      const result = run(command, args)
      if (result.error)
        throw result.error
      if (result.status !== 0)
        throw new Error(`${name}: ${command} ${args.join(' ')} failed (${result.signal || result.status}).`)
    }
  }
}

function sourceIdentity() {
  const git = args => execFileSync('git', args, { cwd: root, encoding: 'utf8' }).trim()
  return {
    commit: git(['rev-parse', 'HEAD']),
    tree: git(['rev-parse', 'HEAD^{tree}']),
    dirty: git(['status', '--porcelain']).length > 0,
  }
}

if (import.meta.main) {
  try {
    const args = process.argv.slice(2)
    const dryRun = args.includes('--dry-run')
    const plan = validationPlan(args.filter(arg => arg !== '--dry-run'))
    console.log(JSON.stringify({ source: sourceIdentity(), suites: plan }, null, 2))
    if (!dryRun) {
      isolateTestCredentials()
      const started = Date.now()
      runPlan(plan, (command, args) => {
        console.log(`\n> ${command} ${args.join(' ')}`)
        return spawnSync(command, args, { cwd: root, stdio: 'inherit' })
      })
      console.log(JSON.stringify({
        result: 'passed',
        source: sourceIdentity(),
        suites: plan.map(suite => suite.name),
        durationMs: Date.now() - started,
      }, null, 2))
    }
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
