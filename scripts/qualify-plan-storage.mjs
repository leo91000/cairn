import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { isolateTestCredentials } from './test-environment.mjs'

if (process.env.GITHUB_ACTIONS !== 'true')
  throw new Error('This two-source qualification runs only on an ephemeral CI runner.')

const reference = process.argv[2]
if (process.argv.length !== 3 || !/^[0-9a-f]{40}$/.test(reference ?? ''))
  throw new Error('Provide the complete reference commit SHA.')

isolateTestCredentials()
const result = spawnSync('python3', [fileURLToPath(new URL('./plan-storage-qualification.py', import.meta.url)), reference], { stdio: 'inherit' })
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
