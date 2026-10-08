import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

async function main() {
  isolateTestCredentials()
  await import('../tests/e2e/android-direct-fixture.ts')
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
