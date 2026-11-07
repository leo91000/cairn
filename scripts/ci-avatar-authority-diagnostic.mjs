import { Buffer } from 'node:buffer'
import { execFileSync, spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  closeSync,
  constants,
  fstatSync,
  openSync,
  readFileSync,
  readSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { isolateTestCredentials } from './test-environment.mjs'

export const AVATAR_FILTER = 'test(agent_avatars::tests::) | test(authorization::tests::) | test(authorization::jobs::tests::)'
export const AVATAR_ARGS = ['scripts/test-backend.mjs', '--nextest', '-p', 'cairn-installation', '--lib', '--test-threads', '8', '--no-fail-fast', '--color', 'never', '--status-level', 'all', '-E', AVATAR_FILTER]
export const WORKER_BUILD_ARGS = ['build', '--locked', '-p', 'cairn-installation', '--bin', 'cairn']
const digest = bytes => createHash('sha256').update(bytes).digest('hex')

export function workerIdentity(file) {
  const fd = openSync(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK)
  try {
    const before = fstatSync(fd)
    if (!before.isFile() || !(before.mode & 0o111) || before.size < 1 || before.size > 512 * 1024 * 1024)
      throw new Error('Invalid candidate worker binary')
    const hash = createHash('sha256')
    const buffer = Buffer.alloc(131072)
    let bytes = 0
    for (;;) {
      const count = readSync(fd, buffer, 0, buffer.length, null)
      if (!count)
        break
      bytes += count
      hash.update(buffer.subarray(0, count))
      if (bytes > before.size)
        throw new Error('Candidate worker changed during hashing')
    }

    const after = fstatSync(fd)
    if (bytes !== before.size || after.size !== before.size || after.mtimeMs !== before.mtimeMs)
      throw new Error('Candidate worker changed during hashing')
    return { bytes, sha256: hash.digest('hex') }
  }
  finally { closeSync(fd) }
}

export function sourceIdentity(source, candidate) {
  const git = args => execFileSync('git', args, { cwd: source, encoding: 'utf8', timeout: 10000 }).trim()
  const commit = git(['rev-parse', 'HEAD'])
  if (!/^[a-f0-9]{40}$/.test(candidate ?? '') || commit !== candidate)
    throw new Error('Avatar diagnostic source mismatch')
  // Existing diagnostic setup inserts only the phase observer in this launcher.
  const changed = git(['diff', '--name-only', 'HEAD']).split('\n').filter(Boolean)
  if (changed.some(file => file !== 'scripts/test-backend.mjs'))
    throw new Error('Avatar diagnostic source has tracked changes')
  return { commit, tree: git(['rev-parse', 'HEAD^{tree}']) }
}

export function runAvatarDiagnostic({
  source,
  candidate,
  output,
  env,
  run = spawnSync,
  identify = sourceIdentity,
}) {
  if (!path.isAbsolute(source) || !path.isAbsolute(output) || env.CARGO_BUILD_TARGET)
    throw new Error('Invalid avatar diagnostic paths or cross-compilation target')
  const revision = identify(source, candidate)
  const commandEnv = { ...env, CARGO_TARGET_DIR: path.join(source, 'target') }
  delete commandEnv.CAIRN_TEST_CODE_WORKER
  delete commandEnv.CAIRN_TEST_CODE_WORKERS
  const execute = (command, args) => {
    const result = run(command, args, { cwd: source, env: commandEnv, stdio: 'inherit' })
    if (result.error || !Number.isInteger(result.status) || result.status < 0 || result.status > 255)
      throw new Error('Avatar diagnostic command did not return a known exit')
    return result.status
  }

  if (env.CAIRN_CI_PHASE_FILE)
    writeFileSync(env.CAIRN_CI_PHASE_FILE, `${JSON.stringify({ phase: 'code-worker-build' })}\n`, { mode: 0o600 })
  const build = execute('cargo', WORKER_BUILD_ARGS)
  writeFileSync(path.join(output, 'avatar-build.json'), `${JSON.stringify({
    diagnosticOnly: true,
    qualificationClaim: false,
    source: revision,
    iteration: 1,
    exitCode: build,
  })}\n`, { flag: 'wx', mode: 0o600 })
  if (build !== 0)
    return build
  identify(source, candidate)
  // --lib does not select an integration-test CARGO_BIN_EXE_cairn. The existing
  // grant fixture falls back here after test-backend deletes worker overrides.
  const worker = workerIdentity(path.join(source, 'target/debug/cairn'))
  const provenance = {
    diagnosticOnly: true,
    qualificationClaim: false,
    selector: 'avatar-authority',
    source: revision,
    worker: { path: 'target/debug/cairn', ...worker },
    launcherSHA256: digest(readFileSync(path.join(source, 'scripts/test-backend.mjs'))),
    fixtureSHA256: digest(readFileSync(path.join(source, 'crates/installation/tests/fixtures/plugin_grants.rs'))),
    avatarTestSHA256: digest(readFileSync(path.join(source, 'crates/installation/src/agent_avatars/tests.rs'))),
    cargoLockSHA256: digest(readFileSync(path.join(source, 'Cargo.lock'))),
    iteration: 1,
  }
  writeFileSync(path.join(output, 'avatar-worker.json'), `${JSON.stringify(provenance)}\n`, { flag: 'wx', mode: 0o600 })
  const result = execute('node', AVATAR_ARGS)
  writeFileSync(path.join(output, 'avatar-result.json'), `${JSON.stringify({
    diagnosticOnly: true,
    qualificationClaim: false,
    iteration: 1,
    exitCode: result,
  })}\n`, { flag: 'wx', mode: 0o600 })
  identify(source, candidate)
  if (workerIdentity(path.join(source, 'target/debug/cairn')).sha256 !== worker.sha256)
    throw new Error('Candidate worker changed during the diagnostic')
  return result
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  isolateTestCredentials()
  try {
    if (process.argv.length !== 2 || process.env.BACKEND_DIAGNOSTIC_SELECTOR !== 'avatar-authority')
      throw new Error('Avatar diagnostic requires its fixed selector')
    process.exitCode = runAvatarDiagnostic({
      source: process.cwd(),
      candidate: process.env.CAIRN_AVATAR_DIAGNOSTIC_CANDIDATE,
      output: process.env.CAIRN_AVATAR_DIAGNOSTIC_OUTPUT,
      env: process.env,
    })
  }
  catch {
    console.error('Avatar diagnostic setup or provenance failed; no qualification claimed.')
    process.exitCode = 125
  }
}
