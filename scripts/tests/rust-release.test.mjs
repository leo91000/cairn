import { execFileSync, spawnSync } from 'node:child_process'
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'

const root = fileURLToPath(new URL('../../', import.meta.url))
const config = readFileSync(path.join(root, '.cargo/config.toml'), 'utf8')
const wrapperPath = config.match(/^rustc-workspace-wrapper = "([^"]+)"$/m)[1]
const wrapper = path.join(root, wrapperPath)
const inline75 = 'llvm-args=--inline-threshold=75 --inlinehint-threshold=75 --locally-hot-callsite-threshold=75'

function invokeWrapper(args, extraEnv = {}) {
  const directory = mkdtempSync(path.join(tmpdir(), 'cairn rustc wrapper '))
  const compiler = path.join(directory, 'compiler.mjs')

  try {
    writeFileSync(compiler, `#!${process.execPath}
console.log(JSON.stringify(process.argv.slice(2)))
if (process.env.FIXTURE_SIGNAL) process.kill(process.pid, process.env.FIXTURE_SIGNAL)
process.exit(Number(process.env.FIXTURE_STATUS || 0))
`)
    chmodSync(compiler, 0o755)

    return spawnSync(wrapper, [compiler, ...args], {
      env: { ...process.env, ...extraEnv },
      encoding: 'utf8',
    })
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

describe('rust release workspace wrapper', () => {
  it.each([
    ['combined codegen arguments', ['-Copt-level=3', '-Ccodegen-units=4']],
    ['separate codegen arguments', ['-C', 'opt-level=3', '-C', 'codegen-units=4']],
    ['underscore aliases', ['-Copt_level=3', '-Ccodegen_units=4']],
    ['long codegen arguments', ['--codegen', 'opt_level=3', '--codegen=codegen_units=4']],
    ['last effective codegen arguments', ['-Copt-level=0', '-Ccodegen-units=1', '-C', 'opt-level=3', '-Ccodegen-units=4']],
  ])('adds the measured inlining settings for %s', (_, codegen) => {
    const args = ['--crate-name', 'fixture', 'source with spaces.rs', ...codegen]
    const result = invokeWrapper(args)

    expect(result.status).toBe(0)
    expect(JSON.parse(result.stdout)).toEqual([...args, '-C', inline75])
  })

  it.each([
    ['development', ['-Cdebuginfo=2']],
    ['dependency release settings', ['-Copt-level=3', '-Ccodegen-units=1']],
    ['build scripts', ['-Copt-level=0', '-Ccodegen-units=4']],
    ['a later optimization override', ['-Copt-level=3', '-Ccodegen-units=4', '-C', 'opt-level=2']],
    ['a later codegen-unit override', ['-Copt-level=3', '-Ccodegen-units=4', '-Ccodegen-units=8']],
    ['a later optimization alias override', ['-Copt-level=3', '-Ccodegen-units=4', '-Copt_level=2']],
    ['a later codegen-unit alias override', ['-Copt-level=3', '-Ccodegen-units=4', '--codegen=codegen_units=8']],
    ['compiler queries', ['--print=cfg', '-Copt-level=3', '-Ccodegen-units=4']],
    ['options supplied through a response file', ['-Copt-level=3', '-Ccodegen-units=4', '@flags with spaces']],
    ['explicit combined LLVM arguments', ['-Copt-level=3', '-Ccodegen-units=4', '-Cllvm-args=--inline-threshold=123']],
    ['explicit separate LLVM arguments', ['-Copt-level=3', '-Ccodegen-units=4', '-C', 'llvm-args=--inline-threshold=123']],
    ['explicit LLVM argument aliases', ['-Copt-level=3', '-Ccodegen-units=4', '-Cllvm_args=--inline-threshold=123']],
  ])('preserves caller arguments for %s', (_, args) => {
    const result = invokeWrapper(args)

    expect(result.status).toBe(0)
    expect(JSON.parse(result.stdout)).toEqual(args)
  })

  it('preserves the compiler exit status and termination signal', () => {
    expect(invokeWrapper([], { FIXTURE_STATUS: '37' }).status).toBe(37)
    expect(invokeWrapper([], { FIXTURE_SIGNAL: 'SIGTERM' }).signal).toBe('SIGTERM')
  })

  it('limits actual Cargo builds to workspace release crates and invalidates their cache with a versioned path', () => {
    const directory = mkdtempSync(path.join(tmpdir(), 'cairn cargo release '))
    const workspace = path.join(directory, 'workspace')
    const callsFile = path.join(directory, 'rustc.jsonl')
    const compiler = path.join(directory, 'record-rustc.mjs')
    const cargo = execFileSync('rustup', ['which', 'cargo'], { cwd: root, encoding: 'utf8' }).trim()
    const rustc = execFileSync('rustup', ['which', 'rustc'], { cwd: root, encoding: 'utf8' }).trim()
    const env = { ...process.env }

    for (const key of Object.keys(env)) {
      if (/^(?:RUSTC|RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_BUILD_|CARGO_TARGET_|CARGO_PROFILE_)/.test(key))
        delete env[key]
    }

    Object.assign(env, {
      CARGO_HOME: path.join(directory, 'cargo-home'),
      CARGO_TARGET_DIR: path.join(directory, 'target'),
      RUSTC: compiler,
      FIXTURE_RUSTC: rustc,
      FIXTURE_CALLS: callsFile,
    })

    const write = (relative, content) => {
      const destination = path.join(directory, relative)
      mkdirSync(path.dirname(destination), { recursive: true })
      writeFileSync(destination, content)
    }

    const build = (release = true) => {
      writeFileSync(callsFile, '')
      const args = ['build', '--offline', '--workspace', '--message-format=json']
      if (release)
        args.push('--release')

      const result = spawnSync(cargo, args, {
        cwd: workspace,
        env,
        encoding: 'utf8',
        timeout: 30000,
      })
      expect(result.error, result.stderr).toBeUndefined()
      expect(result.status, result.stderr).toBe(0)

      return {
        calls: readFileSync(callsFile, 'utf8').trim().split('\n').filter(Boolean).map(line => JSON.parse(line)),
        artifacts: result.stdout.trim().split('\n').map(line => JSON.parse(line)).filter(message => message.reason === 'compiler-artifact'),
      }
    }

    const crateCalls = result => result.calls.filter(args => args.includes('--crate-name') && !args.includes('___'))
    const crateName = args => args[args.indexOf('--crate-name') + 1]

    try {
      write('workspace/Cargo.toml', `[workspace]
members = ["app", "support"]
resolver = "3"

[profile.release]
codegen-units = 1

[profile.release.package.fixture-app]
codegen-units = 4

[profile.release.package.fixture-support]
codegen-units = 4
`)
      write('workspace/app/Cargo.toml', '[package]\nname = "fixture-app"\nversion = "0.0.0"\nedition = "2024"\n\n[dependencies]\nfixture-support = { path = "../support" }\nfixture-external = { path = "../../external" }\n')
      write('workspace/app/src/main.rs', 'fn main() { println!("{}", fixture_support::answer() + fixture_external::answer()); }\n')
      write('workspace/app/build.rs', 'fn main() { println!("cargo:rerun-if-changed=build.rs"); }\n')
      write('workspace/support/Cargo.toml', '[package]\nname = "fixture-support"\nversion = "0.0.0"\nedition = "2024"\n')
      write('workspace/support/src/lib.rs', 'pub fn answer() -> u32 { 20 }\n')
      write('external/Cargo.toml', '[package]\nname = "fixture-external"\nversion = "0.0.0"\nedition = "2024"\n')
      write('external/src/lib.rs', 'pub fn answer() -> u32 { 22 }\n')
      write('workspace/.cargo/config.toml', config)
      write(`workspace/${wrapperPath}`, readFileSync(wrapper, 'utf8'))
      chmodSync(path.join(workspace, wrapperPath), 0o755)
      write('record-rustc.mjs', `#!${process.execPath}
import { spawnSync } from 'node:child_process'
import { appendFileSync } from 'node:fs'
const args = process.argv.slice(2)
appendFileSync(process.env.FIXTURE_CALLS, JSON.stringify(args) + '\\n')
const result = spawnSync(process.env.FIXTURE_RUSTC, args, { stdio: 'inherit' })
if (result.signal) process.kill(process.pid, result.signal)
process.exit(result.status ?? 1)
`)
      chmodSync(compiler, 0o755)

      const release = build()
      const releaseCalls = new Map(crateCalls(release).map(args => [crateName(args), args]))
      expect([...releaseCalls.keys()].sort()).toEqual(['build_script_build', 'fixture_app', 'fixture_external', 'fixture_support'])
      for (const name of ['fixture_app', 'fixture_support']) {
        expect(releaseCalls.get(name)).toContain('codegen-units=4')
        expect(releaseCalls.get(name)).toContain(inline75)
      }

      expect(releaseCalls.get('fixture_external')).toContain('codegen-units=1')
      expect(releaseCalls.get('fixture_external').some(arg => arg.includes('llvm-args='))).toBe(false)
      expect(releaseCalls.get('build_script_build').some(arg => arg.includes('llvm-args='))).toBe(false)
      expect(execFileSync(path.join(env.CARGO_TARGET_DIR, 'release/fixture-app'), { encoding: 'utf8' }).trim()).toBe('42')

      const cached = build()
      expect(crateCalls(cached)).toEqual([])
      expect(cached.artifacts.every(artifact => artifact.fresh)).toBe(true)

      const development = build(false)
      expect(crateCalls(development).some(args => args.some(arg => arg.includes('llvm-args=')))).toBe(false)

      // Cargo keys workspace artifacts on the wrapper pathname, not its contents.
      const modifiedWrapper = readFileSync(wrapper, 'utf8').replaceAll('=75', '=74')
      writeFileSync(path.join(workspace, wrapperPath), modifiedWrapper)
      const samePath = build()
      expect(crateCalls(samePath)).toEqual([])
      expect(samePath.artifacts.every(artifact => artifact.fresh)).toBe(true)

      const nextWrapperPath = 'scripts/rust-release-inline74-v2.sh'
      copyFileSync(path.join(workspace, wrapperPath), path.join(workspace, nextWrapperPath))
      chmodSync(path.join(workspace, nextWrapperPath), 0o755)
      writeFileSync(path.join(workspace, '.cargo/config.toml'), config.replace(wrapperPath, nextWrapperPath))
      const changedPath = build()
      const rebuiltNames = crateCalls(changedPath).map(crateName)
      expect(rebuiltNames).toContain('fixture_app')
      expect(rebuiltNames).toContain('fixture_support')
      expect(rebuiltNames).not.toContain('fixture_external')
      for (const args of crateCalls(changedPath).filter(args => ['fixture_app', 'fixture_support'].includes(crateName(args))))
        expect(args).toContain(inline75.replaceAll('=75', '=74'))
      expect(changedPath.artifacts.find(artifact => artifact.target.name === 'fixture_external').fresh).toBe(true)
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  }, 60000)
})
