import { execFileSync } from 'node:child_process'
import { expect, it } from 'vitest'

it('observes archived native admission failures without changing the fixture API or exposing payloads', () => {
  const output = execFileSync('python3', ['-B', 'scripts/tests/test_native_admission_diagnostics.py'], {
    encoding: 'utf8',
    timeout: 10000,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  expect(output).toBe('')
})
