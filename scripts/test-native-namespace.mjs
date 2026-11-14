import { spawnSync } from 'node:child_process'
import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

isolateTestCredentials()
const built = spawnSync('cargo', ['test', '--locked', '--workspace', '--lib', '--no-run', '--message-format=json'], {
  encoding: 'utf8',
  stdio: ['ignore', 'pipe', 'inherit'],
  maxBuffer: 64 * 1024 * 1024,
})
if (built.error)
  throw built.error
const records = built.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line))
for (const record of records) {
  if (record.reason === 'compiler-message' && record.message?.rendered)
    process.stderr.write(record.message.rendered)
}

if (built.status !== 0)
  process.exit(built.status ?? 1)
const binary = records.find(record => record.reason === 'compiler-artifact' && record.target?.name === 'cairn_installation' && record.executable)?.executable
if (!binary)
  throw new Error('Cargo did not produce the isolated namespace test executable')
const privileged = process.argv.includes('--sudo')
const testNames = ['storage::ublk::namespace::tests::isolated_namespace_qualification']
if (process.argv.includes('--retirement') && !privileged)
  throw new Error('Physical retirement qualification requires --sudo and a disposable ublk host')
if (process.argv.includes('--retirement'))
  testNames.push('storage::ublk::tests::isolated_retirement_qualification')
const listed = spawnSync(binary, ['--list', '--ignored'], { encoding: 'utf8' })
if (listed.error)
  throw listed.error
if (listed.status !== 0 || testNames.some(name => !listed.stdout.split('\n').includes(`${name}: test`)))
  throw new Error('Namespace qualification test is absent from the executable')
const command = privileged ? 'sudo' : 'unshare'
for (const testName of testNames) {
  const args = privileged ? ['-n', 'unshare', '--mount', '--propagation', 'private'] : ['--user', '--map-root-user', '--mount', '--propagation', 'private']
  args.push(binary, '--exact', testName, '--ignored', '--nocapture')
  const tested = spawnSync(command, args, { stdio: 'inherit', timeout: 60000, killSignal: 'SIGKILL' })
  if (tested.error)
    throw tested.error
  if (tested.status !== 0)
    process.exit(tested.status ?? 1)
}
