import { defineConfig } from 'vitest/config'
import { isolateTestCredentials } from './scripts/test-environment.mjs'

isolateTestCredentials()

export default defineConfig({
  test: { include: ['scripts/tests/**/*.test.{ts,mjs}', 'apps/web/tests/**/*.test.ts'], testTimeout: 15000, pool: 'forks' },
})
