import {
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { validationScope } from '../pr-validation-scope.mjs'
import { runPlan, validationPlan } from '../validate-pr.mjs'

describe('pR validation', () => {
  it('includes platform and contract checks for indirect shared inputs', () => {
    for (const file of ['crates/contracts/src/lib.rs', 'packages/contracts/generated/models.json', 'scripts/generate-contracts.mjs', 'tests/fixtures/resume-contracts.json', 'pnpm-lock.yaml', '.github/workflows/pr.yaml', 'scripts/pr-validation-scope.mjs', 'apps/android/app/src/main/java/build/cairn/contracts/ResumeRunClient.kt', 'apps/android/app/src/main/java/build/cairn/boundary/Wire.kt'])
      expect(validationScope([file])).toEqual({ android: true, contracts: true })
    expect(validationScope(['apps/web/src/main.ts'])).toEqual({ android: false, contracts: false })
    expect(validationScope(['apps/android/app/build.gradle.kts'])).toEqual({ android: true, contracts: false })
    expect(validationScope(['crates/installation/src/lib.rs'])).toEqual({ android: false, contracts: true })
    expect(validationScope(['rust-toolchain.toml'])).toEqual({ android: false, contracts: true })
    expect(validationScope(['scripts/backend-schemas.mjs'])).toEqual({ android: false, contracts: true })
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
