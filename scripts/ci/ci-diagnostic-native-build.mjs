import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  copyFileSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'

const manifest = JSON.parse(readFileSync('native-diagnostics/source.json', 'utf8'))
for (const role of ['reference', 'candidate']) {
  const source = manifest.sources[role]
  const sha = manifest.revisions[role]
  const actual = execFileSync('git', ['rev-parse', 'HEAD'], { cwd: source, encoding: 'utf8' }).trim()
  if (actual !== sha)
    throw new Error('Diagnostic build source changed')
  const target = path.join(process.env.RUNNER_TEMP, `diagnostic-build-${role}`)
  execFileSync('cargo', ['+1.97.1', 'build', '--locked', '--manifest-path', path.join(source, 'Cargo.toml'), '--bin', 'cairn', '--example', 'native_storage_qualifier'], {
    stdio: 'inherit',
    env: { ...process.env, CARGO_TARGET_DIR: target },
  })
  const destination = role === 'reference' ? 'reference-native' : 'target/debug'
  const binaries = ['cairn', 'examples/native_storage_qualifier']
  mkdirSync(path.join(destination, 'examples'), { recursive: true })
  const checksums = {}
  for (const binary of binaries) {
    const file = path.join(destination, binary)
    copyFileSync(path.join(target, 'debug', binary), file)
    checksums[binary] = createHash('sha256').update(readFileSync(file)).digest('hex')
  }

  const metadata = {
    headSha: sha,
    profile: 'development, debug=0, incremental=0',
    features: 'default (ublk required)',
    rust: '1.97.1',
    image: process.env.ImageVersion,
    buildRun: process.env.GITHUB_RUN_ID,
    sha256: checksums,
  }
  const evidence = path.join('native-storage-evidence', role)
  mkdirSync(evidence, { recursive: true })
  for (const directory of [destination, evidence])
    writeFileSync(path.join(directory, 'revision.json'), `${JSON.stringify(metadata)}\n`)
}
