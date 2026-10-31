import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { expect, it } from 'vitest'

it('keeps source-host log aggregation and completed-page watermarks bounded', () => {
  const output = execFileSync('python3', [fileURLToPath(new URL('./log_coverage_test.py', import.meta.url))], { encoding: 'utf8', timeout: 10000, stdio: ['ignore', 'pipe', 'pipe'] })
  expect(output).toBe('')
})
