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

const target = path.resolve('target')
mkdirSync(target, { recursive: true })
const fixture = mkdtempSync(path.join(target, 'quota-qualification-'))
const tests = ['native::quota::tests::kernel_quota_lifecycle_qualification', 'native::bases_tests::kernel_prepared_base_admission_and_recovery', 'native::bases_tests::measure_prepared_native_bases', 'native::directory::tests::kernel_missing_conversion_replacement_preserves_original_journal']
const script = `
set -euo pipefail
binary=$1
fixture=$2
shift 2
cleanup() {
  result=$?
  trap - EXIT
  if mountpoint -q "$fixture/state/native-data"; then
    if ! umount -- "$fixture/state/native-data"; then
      echo 'Native quota fixture unmount is unconfirmed; owned image retained.' >&2
      exit 1
    fi
  fi
  rm -f -- "$fixture/quota.ext4" "$fixture/test.log"
  rm -rf -- "$fixture/state"
  rmdir -- "$fixture/home" "$fixture"
  exit "$result"
}
trap cleanup EXIT
mkdir -p -- "$fixture/state/native-data" "$fixture/home"
truncate -s 1G -- "$fixture/quota.ext4"
mkfs.ext4 -q -F -O project,quota -E quotatype=prjquota "$fixture/quota.ext4"
mount -o loop,prjquota -- "$fixture/quota.ext4" "$fixture/state/native-data"
for test_name in "$@"; do
env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME="$fixture/home" \\
  CAIRN_NODE_BACKEND=native RUNNER_STATE_DIR="$fixture/state" \\
  CAIRN_NATIVE_DISPOSABLE_HOST=1 CAIRN_NATIVE_QUOTA_TEST_STATE="$fixture/state" \\
  timeout --signal=TERM --kill-after=10s 180s \\
  setpriv --bounding-set=-sys_resource "$binary" --exact "$test_name" --ignored --nocapture --test-threads=1 | tee "$fixture/test.log"
grep -Eq '^test result: ok[.] 1 passed; 0 failed; 0 ignored;' "$fixture/test.log"
done
`
const result = spawnSync('sudo', ['-n', 'unshare', '--mount', '--propagation', 'private', 'bash', '-c', script, 'cairn-quota-qualification', binary, fixture, ...tests], {
  stdio: 'inherit',
})
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
