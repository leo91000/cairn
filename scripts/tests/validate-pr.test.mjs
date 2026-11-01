import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import {
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { validationFailures } from '../pr-validation-required.mjs'
import { validationScope } from '../pr-validation-scope.mjs'
import { runPlan, validationPlan } from '../validate-pr.mjs'

const requiredValidationCli = fileURLToPath(new URL('../pr-validation-required.mjs', import.meta.url))

function runRequiredValidationCli(results) {
  return spawnSync(process.execPath, [requiredValidationCli], {
    encoding: 'utf8',
    env: { RESULTS: JSON.stringify(results) },
  })
}

describe('pR validation', () => {
  it('includes platform and contract checks for indirect shared inputs', () => {
    for (const file of ['crates/contracts/src/lib.rs', 'packages/contracts/generated/models.json', 'scripts/generate-contracts.mjs', 'tests/fixtures/resume-contracts.json', 'pnpm-lock.yaml', '.github/workflows/pr.yaml', 'scripts/pr-validation-scope.mjs'])
      expect(validationScope([file])).toEqual({ android: true, contracts: true, browser: true })
    expect(validationScope(['apps/web/src/main.ts'])).toEqual({ android: false, contracts: false, browser: true })
    expect(validationScope(['apps/android/app/src/main/java/build/cairn/contracts/ResumeRunClient.kt'])).toEqual({ android: true, contracts: true, browser: false })
    expect(validationScope(['apps/android/app/src/main/java/build/cairn/boundary/Wire.kt'])).toEqual({ android: true, contracts: true, browser: false })
    expect(validationScope(['apps/android/app/build.gradle.kts'])).toEqual({ android: true, contracts: false, browser: false })
    expect(validationScope(['crates/installation/src/lib.rs'])).toEqual({ android: false, contracts: true, browser: true })
    expect(validationScope(['rust-toolchain.toml'])).toEqual({ android: false, contracts: true, browser: true })
    expect(validationScope(['scripts/generate-contracts.mjs'])).toEqual({ android: true, contracts: true, browser: true })
    expect(validationScope(['tests/e2e/chats.spec.ts'])).toEqual({ android: false, contracts: false, browser: true })
    expect(validationScope(['docs/README.md'])).toEqual({ android: false, contracts: false, browser: false })
  })

  it('accepts only literal scope booleans and matches every selected job result', () => {
    const allSelected = {
      scope: { result: 'success', outputs: { contracts: 'true', android: 'true', browser: 'true' } },
      quality: { result: 'success' },
      contracts: { result: 'success' },
      android: { result: 'success' },
      browser: { result: 'success' },
    }
    const noneSelected = {
      scope: { result: 'success', outputs: { contracts: 'false', android: 'false', browser: 'false' } },
      quality: { result: 'success' },
      contracts: { result: 'skipped' },
      android: { result: 'skipped' },
      browser: { result: 'skipped' },
    }

    expect(validationFailures(allSelected)).toEqual([])
    expect(validationFailures(noneSelected)).toEqual([])
    expect(validationFailures({ ...allSelected, browser: { result: 'failure' } }))
      .toContain('browser: expected success, got failure')
    expect(validationFailures({ ...noneSelected, android: { result: 'failure' } }))
      .toContain('android: expected skipped, got failure')
    expect(validationFailures({ ...allSelected, scope: { ...allSelected.scope, result: 'failure' } }))
      .toContain('scope did not pass')
    expect(validationFailures({ ...allSelected, quality: { result: 'failure' } }))
      .toContain('quality did not pass')

    const invalidSelections = [
      ['missing', undefined],
      ['null', null],
      ['boolean false', false],
      ['boolean true', true],
      ['case mismatch', 'True'],
      ['unknown string', 'unselected'],
    ]
    for (const name of ['contracts', 'android', 'browser']) {
      for (const [label, value] of invalidSelections) {
        const outputs = { ...allSelected.scope.outputs }
        if (value === undefined)
          delete outputs[name]
        else
          outputs[name] = value
        const needs = {
          ...allSelected,
          scope: { ...allSelected.scope, outputs },
          [name]: { result: 'skipped' },
        }
        expect(validationFailures(needs), `${name}: ${label}`)
          .toContain(`${name}: scope output must be exactly "true" or "false"`)
      }
    }
  })

  it('validates fixed job results through the actual JSON environment CLI entrypoint', () => {
    const allSelected = {
      scope: { result: 'success', outputs: { contracts: 'true', android: 'true', browser: 'true' } },
      quality: { result: 'success' },
      contracts: { result: 'success' },
      android: { result: 'success' },
      browser: { result: 'success' },
    }
    const noneSelected = {
      scope: { result: 'success', outputs: { contracts: 'false', android: 'false', browser: 'false' } },
      quality: { result: 'success' },
      contracts: { result: 'skipped' },
      android: { result: 'skipped' },
      browser: { result: 'skipped' },
    }
    expect(runRequiredValidationCli(allSelected).status).toBe(0)
    expect(runRequiredValidationCli(noneSelected).status).toBe(0)

    const malformed = {
      ...noneSelected,
      scope: { ...noneSelected.scope, outputs: { ...noneSelected.scope.outputs, browser: 'False' } },
    }
    const malformedResult = runRequiredValidationCli(malformed)
    expect(malformedResult.status).toBe(1)
    expect(malformedResult.stderr).toContain('browser: scope output must be exactly "true" or "false"')

    const missing = { ...noneSelected, scope: { result: 'success', outputs: {} } }
    expect(runRequiredValidationCli(missing).status).toBe(1)
    const failedScope = { ...allSelected, scope: { ...allSelected.scope, result: 'failure' } }
    expect(runRequiredValidationCli(failedScope).status).toBe(1)
  })

  it('defaults to the full core and validates all suite names before running anything', () => {
    expect(validationPlan([]).map(suite => suite.name)).toEqual(['quality', 'contracts', 'rust', 'backend'])
    expect(() => validationPlan(['quality', 'typo'])).toThrow('Unknown validation suite')
    expect(() => validationPlan(['toString'])).toThrow('Unknown validation suite')
    expect(validationPlan(['all', 'android']).map(suite => suite.name)).toEqual(['quality', 'contracts', 'rust', 'backend', 'android', 'browser'])
  })

  it('stops at the first failed or interrupted command without reporting later suites as passed', () => {
    for (const result of [{ status: 2 }, { status: null, signal: 'SIGTERM' }, { error: new Error('missing executable') }]) {
      const run = vi.fn().mockReturnValue(result)
      expect(() => runPlan(validationPlan(['quality', 'backend']), run)).toThrow()
      expect(run).toHaveBeenCalledOnce()
    }
  })

  it('uses the isolated backend launcher for normal and documentation tests', () => {
    const run = vi.fn().mockReturnValue({ status: 0 })
    runPlan(validationPlan(['backend']), run)
    expect(run.mock.calls).toEqual([['pnpm', ['test:backend']], ['pnpm', ['test:backend', '--doc']]])
  })
})
