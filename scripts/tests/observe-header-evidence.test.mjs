import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { expect, it } from 'vitest'

it('requires complete fresh header observations, actual-clock reserves and closed descendants', () => {
  const output = execFileSync(process.execPath, ['--test', '--test-reporter=tap', fileURLToPath(new URL('./observe-header-evidence.node.mjs', import.meta.url))], {
    encoding: 'utf8',
    timeout: 10000,
    stdio: ['ignore', 'pipe', 'pipe'],
  })
  expect(output).toContain('# tests 6')
  expect(output).toContain('# pass 6')
  expect(output).toContain('# fail 0')
  expect(output).toContain('# skipped 0')
})
