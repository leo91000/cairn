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
const testNames = [
  'storage::ublk::namespace::tests::isolated_namespace_qualification',
  'native::block::holders::tests::retained_namespace_without_procfs_still_reports_the_device',
  'native::block::holders::tests::private_mount_outside_a_chroot_is_not_lost',
  'native::block::holders::tests::covered_namespace_root_requires_independent_superblock_inventory',
  'native::block::holders::tests::covered_network_namespace_does_not_block_filesystem_inventory',
]
// A user namespace cannot create a block node on the host-backed temp directory.
if (privileged) {
  testNames.push('native::block::holders::tests::raw_block_device_descriptor_is_a_holder', 'native::block::holders::tests::mapped_raw_block_node_is_a_holder_after_its_descriptor_closes')
}

const listed = spawnSync(binary, ['--list', '--ignored'], { encoding: 'utf8' })
if (listed.error)
  throw listed.error
if (listed.status !== 0 || testNames.some(testName => !listed.stdout.split('\n').includes(`${testName}: test`)))
  throw new Error('Namespace qualification test is absent from the executable')
const command = privileged ? 'sudo' : 'unshare'
const args = privileged ? ['-n', 'unshare', '--mount', '--propagation', 'private'] : ['--user', '--map-root-user', '--mount', '--propagation', 'private']
for (const testName of testNames) {
  const tested = spawnSync(command, [...args, binary, '--exact', testName, '--ignored', '--nocapture'], { stdio: 'inherit' })
  if (tested.error)
    throw tested.error
  if (tested.status !== 0)
    process.exit(tested.status ?? 1)
}
