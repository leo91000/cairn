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
const test = 'native::quota::tests::kernel_quota_lifecycle_qualification'
const script = `
set -euo pipefail
binary=$1
fixture=$2
test_name=$3
cleanup() {
  result=$?
  trap - EXIT
  if mountpoint -q "$fixture/state"; then
    if ! umount -- "$fixture/state"; then
      echo 'Native quota fixture unmount is unconfirmed; owned image retained.' >&2
      exit 1
    fi
  fi
  rm -f -- "$fixture/quota.ext4"
  rmdir -- "$fixture/state" "$fixture/home" "$fixture"
  exit "$result"
}
trap cleanup EXIT
mkdir -- "$fixture/state" "$fixture/home"
truncate -s 1G -- "$fixture/quota.ext4"
mkfs.ext4 -q -F -O project,quota -Q prjquota "$fixture/quota.ext4"
mount -o loop,prjquota -- "$fixture/quota.ext4" "$fixture/state"
env -i PATH=/usr/sbin:/usr/bin:/sbin:/bin HOME="$fixture/home" \\
  CAIRN_NATIVE_DISPOSABLE_HOST=1 CAIRN_NATIVE_QUOTA_TEST_STATE="$fixture/state" \\
  timeout --signal=TERM --kill-after=10s 180s \\
  "$binary" --exact "$test_name" --ignored --nocapture --test-threads=1
`
const result = spawnSync('sudo', ['-n', 'unshare', '--mount', '--propagation', 'private', 'bash', '-c', script, 'cairn-quota-qualification', binary, fixture, test], {
  stdio: 'inherit',
})
if (result.error)
  throw result.error
process.exit(result.status ?? 1)
