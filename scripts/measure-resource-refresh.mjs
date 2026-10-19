import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

async function main() {
  // Isolate before importing fixture modules, as the backend/device launchers do.
  isolateTestCredentials()
  delete process.env.GH_TOKEN
  delete process.env.GITHUB_TOKEN
  delete process.env.CAIRN_NETWORK_INSTALLATION_BINARY
  delete process.env.CAIRN_DIRECT_PUBLIC_IP
  await import('../tests/measurements/refresh.ts')
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
