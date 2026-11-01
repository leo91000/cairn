import {
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { validationFailures } from '../pr-validation-required.mjs'
import { validationScope } from '../pr-validation-scope.mjs'
import { runPlan, validationPlan } from '../validate-pr.mjs'

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

  it('requires the selected browser job and permits only a genuinely unselected skip', () => {
    const required = {
      scope: { result: 'success', outputs: { contracts: 'false', android: 'false', browser: 'true' } },
      quality: { result: 'success' },
      contracts: { result: 'skipped' },
      android: { result: 'skipped' },
      browser: { result: 'success' },
    }
    expect(validationFailures(required)).toEqual([])
    expect(validationFailures({ ...required, browser: { result: 'failure' } }))
      .toContain('browser: expected success, got failure')
    expect(validationFailures({
      ...required,
      scope: { ...required.scope, outputs: { ...required.scope.outputs, browser: 'false' } },
      browser: { result: 'skipped' },
    })).toEqual([])
    expect(validationFailures({
      ...required,
      scope: { ...required.scope, outputs: { ...required.scope.outputs, browser: 'false' } },
      browser: { result: 'failure' },
    })).toContain('browser: expected skipped, got failure')
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
