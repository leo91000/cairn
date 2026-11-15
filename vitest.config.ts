import { availableParallelism } from 'node:os'
import { defineConfig } from 'vitest/config'
import { isolateTestCredentials } from './scripts/test-environment.mjs'

isolateTestCredentials()

export default defineConfig({
  test: {
    include: ['scripts/tests/**/*.test.{ts,mjs}', 'apps/web/tests/**/*.test.ts'],
    testTimeout: 15000,
    pool: 'forks',
    // Process lifecycle fixtures spawn their own helpers under real deadlines.
    // Bound file concurrency so large hosts retain scheduling and I/O headroom.
    maxWorkers: Math.min(4, availableParallelism()),
  },
})
