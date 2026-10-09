import process from 'node:process'
import { defineConfig } from '@playwright/test'
import { isolateTestCredentials } from './scripts/test-environment.mjs'

isolateTestCredentials()

export default defineConfig({
  testDir: './tests/e2e',
  testMatch: 'network-relay.spec.ts',
  workers: 1,
  retries: 0,
  timeout: 120000,
  expect: { timeout: 10000 },
  outputDir: process.env.CAIRN_NETWORK_PLAYWRIGHT_OUTPUT,
  reporter: [['list']],
})
