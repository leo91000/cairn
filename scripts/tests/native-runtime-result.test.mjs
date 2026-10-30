import { execFileSync } from 'node:child_process'
import { it } from 'vitest'

it('validates native controller exit records and wait results without a privileged host', () => {
  execFileSync('python3', ['-B', 'scripts/tests/native_runtime_result_test.py'], { stdio: 'pipe' })
})
