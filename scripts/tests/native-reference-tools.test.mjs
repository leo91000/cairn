import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { expect, it } from 'vitest'

it('validates native reference tool preparation and private cache admission', () => {
  const script = fileURLToPath(new URL('./native_reference_tool_cache_test.py', import.meta.url))
  const output = execFileSync('python3', ['-B', script], { encoding: 'utf8', stdio: 'pipe' })
  expect(output).toContain('coldReferenceColdCandidate')
})
