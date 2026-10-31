import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { expect, it } from 'vitest'

it('preserves evidence and returns bounded fallback results for invalid archive inputs', () => {
  const output = execFileSync(process.execPath, ['--test', '--test-reporter=tap', fileURLToPath(new URL('./health-evidence-retention.node.mjs', import.meta.url))], {
    encoding: 'utf8',
    timeout: 10000,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  expect(output).toContain('# fail 0')
})
