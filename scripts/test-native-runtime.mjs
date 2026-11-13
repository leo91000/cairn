import { spawnSync } from 'node:child_process'
import path from 'node:path'
import process from 'node:process'
import { parseArgs } from 'node:util'
import { isolateTestCredentials } from './test-environment.mjs'

const { values } = parseArgs({
  options: {
    'repo': { type: 'string', default: process.cwd() },
    'native-runtime': { type: 'string' },
    'qualifier': { type: 'string' },
    'reference': { type: 'boolean', default: false },
  },
})
if (!values['native-runtime'] || !values.qualifier)
  throw new Error('Provide --native-runtime and --qualifier binaries.')

const binary = path.resolve(values['native-runtime'])
const qualifier = path.resolve(values.qualifier)
const repo = path.resolve(values.repo)
isolateTestCredentials()
for (const key of Object.keys(process.env)) {
  if (key.startsWith('AWS_') || key.startsWith('STORAGE_S3_') || key.startsWith('ARCHIVE_'))
    delete process.env[key]
}

if (values.reference) {
  // The previous local-disk fixture still requires these canaries at startup.
  // No server or bucket is created, and the workload requires zero remote reads.
  process.env.CAIRN_NODE_TEST_S3_ENDPOINT = 'http://127.0.0.1:1'
  process.env.AWS_ACCESS_KEY_ID = 'node-fixture'
  process.env.AWS_SECRET_ACCESS_KEY = 'node-fixture-secret'
  process.env.AWS_ENDPOINT_URL_S3 = 'http://127.0.0.1:1'
  process.env.AWS_REGION = 'us-east-1'
  process.env.AWS_EC2_METADATA_DISABLED = 'true'
  process.env.AWS_CONFIG_FILE = '/dev/null'
  process.env.AWS_SHARED_CREDENTIALS_FILE = '/dev/null'
}

const result = spawnSync('python3', [path.join(repo, 'scripts/tests/native_runtime_test.py'), binary, qualifier], {
  cwd: repo,
  stdio: 'inherit',
})
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
