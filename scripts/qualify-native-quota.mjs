import { spawnSync } from 'node:child_process'
import { mkdirSync, mkdtempSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { isolateTestCredentials } from './test-environment.mjs'

if (process.env.CAIRN_NATIVE_DISPOSABLE_HOST !== '1')
  throw new Error('Native quota qualification requires an explicitly designated disposable host.')

isolateTestCredentials()
const built = spawnSync('cargo', ['test', '--locked', '--workspace', '--lib', '--no-run', '--message-format=json'], {
  encoding: 'utf8',
  stdio: ['ignore', 'pipe', 'inherit'],
  maxBuffer: 64 * 1024 * 1024,
})
if (built.error)
  throw built.error
if (built.status !== 0)
  process.exit(built.status ?? 1)

const records = built.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line))
const binary = records.find(record => record.reason === 'compiler-artifact' && record.target?.name === 'cairn_installation' && record.executable)?.executable
if (!binary)
  throw new Error('Cargo did not produce the isolated quota qualification executable.')
// migrate-pool drives the installed runtime's offline pool subcommand.
const runtime = spawnSync('cargo', ['build', '--locked', '-p', 'cairn-installation', '--bin', 'cairn', '--message-format=json'], {
  encoding: 'utf8',
  stdio: ['ignore', 'pipe', 'inherit'],
  maxBuffer: 64 * 1024 * 1024,
})
if (runtime.error)
  throw runtime.error
if (runtime.status !== 0)
  process.exit(runtime.status ?? 1)
const cairn = runtime.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line)).find(record => record.reason === 'compiler-artifact' && record.target?.name === 'cairn' && record.executable)?.executable
if (!cairn)
  throw new Error('Cargo did not produce the native runtime used by the pool migration qualification.')

const target = path.resolve('target')
mkdirSync(target, { recursive: true })
const tests = ['native::quota::tests::kernel_quota_lifecycle_qualification', 'native::bases_tests::kernel_prepared_base_admission_and_recovery', 'native::bases_tests::measure_prepared_native_bases', 'native::directory::tests::kernel_missing_conversion_replacement_preserves_original_journal', 'native::directory::tests::kernel_migration_copy_headroom']
const privilegedWriterTest = 'native::ingress::tests::kernel_privileged_parent_cannot_bypass_writer_quotas'
const idmappedViewTest = 'native::identity::mounts::tests::kernel_mapped_view_keeps_disk_owners_and_uses_conversation_ids'
const spareClaimTest = 'native::spares::tests::kernel_spare_claim_recovers_after_rename_before_quota_adoption'
const reflinkReserveTest = 'native::bases::payload_size_tests::kernel_reflink_admission_excludes_shared_extents_from_physical_reserve'
// Every quota case runs on both supported pool filesystems: ext4, and XFS
// with reflink as recommended for new nodes.
const script = `
set -euo pipefail
binary=$1
fixture=$2
filesystem=$3
shift 3
cleanup() {
  result=$?
  trap - EXIT
  if mountpoint -q "$fixture/state/native-data"; then
    if ! umount -- "$fixture/state/native-data"; then
      echo 'Native quota fixture unmount is unconfirmed; owned image retained.' >&2
      exit 1
    fi
  fi
  rm -f -- "$fixture/quota.$filesystem" "$fixture/test.log"
  rm -rf -- "$fixture/state"
  rmdir -- "$fixture/home" "$fixture"
  exit "$result"
}
trap cleanup EXIT
mkdir -p -- "$fixture/state/native-data" "$fixture/home"
truncate -s 1G -- "$fixture/quota.$filesystem"
if [ "$filesystem" = xfs ]; then
  mkfs.xfs -q -f -m crc=1,reflink=1 -i projid32bit=1 "$fixture/quota.xfs"
else
  mkfs.ext4 -q -F -O project,quota -E quotatype=prjquota "$fixture/quota.ext4"
fi
mount -o loop,prjquota -- "$fixture/quota.$filesystem" "$fixture/state/native-data"
for test_name in "$@"; do
quota_command=(setpriv --bounding-set=-sys_resource)
if [ "$test_name" = "${privilegedWriterTest}" ]; then
  quota_command=()
fi
env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME="$fixture/home" \\
  CAIRN_NODE_BACKEND=native RUNNER_STATE_DIR="$fixture/state" \\
  CAIRN_NATIVE_DISPOSABLE_HOST=1 CAIRN_NATIVE_QUOTA_TEST_STATE="$fixture/state" \\
  timeout --signal=TERM --kill-after=10s 180s \\
  "\${quota_command[@]}" "$binary" --exact "$test_name" --ignored --nocapture --test-threads=1 | tee "$fixture/test.log"
grep -Eq '^test result: ok[.] 1 passed; 0 failed; 0 ignored;' "$fixture/test.log"
done
`
for (const filesystem of ['ext4', 'xfs']) {
  const fixture = mkdtempSync(path.join(target, `quota-qualification-${filesystem}-`))
  const selected = [...tests, privilegedWriterTest, idmappedViewTest, spareClaimTest]
  if (filesystem === 'xfs')
    selected.push(reflinkReserveTest)
  const result = spawnSync('sudo', ['-n', 'unshare', '--mount', '--propagation', 'private', 'bash', '-c', script, 'cairn-quota-qualification', binary, fixture, filesystem, ...selected], {
    stdio: 'inherit',
  })
  if (result.error)
    throw result.error
  if (result.status !== 0)
    process.exit(result.status ?? 1)
}

// The migrated pool is mounted by systemd units, so this case runs in the
// host mount namespace with its own fixture paths and unit names.
const fixtureDirectory = spawnSync('sudo', ['-n', 'mktemp', '-d', '/var/lib/cairn-pool-migration-XXXXXXXX'], {
  encoding: 'utf8',
  stdio: ['ignore', 'pipe', 'inherit'],
})
if (fixtureDirectory.error)
  throw fixtureDirectory.error
if (fixtureDirectory.status !== 0)
  process.exit(fixtureDirectory.status ?? 1)
const migration = fixtureDirectory.stdout.trim()
const migrated = spawnSync('sudo', ['-n', 'env', 'CAIRN_NATIVE_DISPOSABLE_HOST=1', 'python3', '-B', 'scripts/tests/native_pool_migration_qualification.py', binary, cairn, migration], {
  stdio: 'inherit',
})
if (migrated.error)
  throw migrated.error
process.exit(migrated.status ?? 1)
