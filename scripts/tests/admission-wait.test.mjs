import { execFileSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { expect, it } from 'vitest'

function expectNodeTapSuitePasses(output, suitePath) {
  const counts = Object.fromEntries([...output.matchAll(/^# (tests|pass|fail|cancelled|skipped|todo) (\d+)$/gm)]
    .map(([, name, count]) => [name, Number(count)]))
  const passedCases = [...output.matchAll(/^ok \d+ - (.+)$/gm)]
    .filter(([, name]) => name !== suitePath && !/ # (?:SKIP|TODO)(?: |$)/.test(name))

  expect(counts.tests).toBeGreaterThan(0)
  expect(counts.pass).toBeGreaterThan(0)
  expect(counts.fail).toBe(0)
  expect(counts.cancelled).toBe(0)
  expect(counts.skipped).toBe(0)
  expect(counts.todo).toBe(0)
  expect(counts.tests).toBe(counts.pass + counts.fail + counts.cancelled + counts.skipped + counts.todo)
  expect(passedCases.length).toBeGreaterThan(0)
}

it('rejects empty or all-skipped Node TAP child suites', () => {
  const fixtureDir = mkdtempSync(join(tmpdir(), 'cairn-admission-wait-tap-'))
  const emptySuitePath = join(fixtureDir, 'empty.mjs')
  const allSkippedSuitePath = join(fixtureDir, 'all-skipped.mjs')
  const allSkippedSource = [
    'import test from \'node:test\'',
    'test.skip(\'intentional control skip\', () => {})',
    '',
  ].join('\n')

  try {
    writeFileSync(emptySuitePath, '')
    writeFileSync(allSkippedSuitePath, allSkippedSource)

    const emptyOutput = execFileSync(process.execPath, ['--test', '--test-reporter=tap', emptySuitePath], {
      encoding: 'utf8',
      timeout: 10000,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    expect(emptyOutput).toContain(`# Subtest: ${emptySuitePath}`)
    expect(emptyOutput).toContain('# fail 0')
    expect(() => expectNodeTapSuitePasses(emptyOutput, emptySuitePath)).toThrow()

    const allSkippedOutput = execFileSync(process.execPath, ['--test', '--test-reporter=tap', allSkippedSuitePath], {
      encoding: 'utf8',
      timeout: 10000,
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    expect(allSkippedOutput).toMatch(/^# skipped [1-9]\d*$/m)
    expect(allSkippedOutput).toContain('# fail 0')
    expect(() => expectNodeTapSuitePasses(allSkippedOutput, allSkippedSuitePath)).toThrow()
  }
  finally {
    rmSync(fixtureDir, { recursive: true, force: true })
  }
})

it('tracks only the observed current admission wait without modifying evidence state', () => {
  const suitePath = fileURLToPath(new URL('./admission-wait.node.mjs', import.meta.url))
  const output = execFileSync(process.execPath, ['--test', '--test-reporter=tap', suitePath], {
    encoding: 'utf8',
    timeout: 10000,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  expect(output).toContain('# fail 0')
  expectNodeTapSuitePasses(output, suitePath)
})
