// Executed inside the exact candidate image by container-smoke.mjs.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import {
  lstat,
  mkdir,
  mkdtemp,
  readFile,
  realpath,
  rm,
  symlink,
  writeFile,
} from 'node:fs/promises'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'

async function main() {
  const directory = await mkdtemp(path.join(tmpdir(), 'leo-toolkit-probe-'))
  const stale = path.join(process.env.HOME, '.local/share/mise/installs/node/0.0.0')
  await mkdir(path.dirname(stale), { recursive: true })
  await symlink('/usr/local/share/mise/installs/node/0.0.0', stale)
  const env = JSON.parse(execFileSync('/usr/local/bin/leo', ['toolkit-env'], { encoding: 'utf8', timeout: 60000 }))
  await assert.rejects(lstat(stale), { code: 'ENOENT' })
  const run = (binary, args = ['--version'], cwd = directory) => execFileSync(binary, args, {
    env,
    cwd,
    encoding: 'utf8',
    timeout: 120000,
  }).trim()
  try {
    const manifest = JSON.parse(await readFile('/opt/leo-toolkit/manifest.json', 'utf8'))
    assert.ok(run('java').includes(manifest.tools.java.replace('temurin-', '').split('+')[0]))
    assert.equal(env.ANDROID_HOME, path.join(process.env.HOME, '.local/share/android/sdk'))
    assert.equal(env.GRADLE_USER_HOME, path.join(process.env.HOME, '.gradle'))
    assert.equal(JSON.parse(run('leo-android', ['status'])).sdkInstalled, false)
    for (const [tool, binary] of Object.entries({
      'node': 'node',
      'pnpm': 'pnpm',
      'python': 'python',
      'uv': 'uv',
      'ripgrep': 'rg',
      'fd': 'fd',
      'jq': 'jq',
      'yq': 'yq',
      'ast-grep': 'ast-grep',
      'shellcheck': 'shellcheck',
      'shfmt': 'shfmt',
      'actionlint': 'actionlint',
      'just': 'just',
      'hyperfine': 'hyperfine',
      'delta': 'delta',
      'bat': 'bat',
      'ruff': 'ruff',
      'go': 'go',
      'rust': 'rustc',
      'cmake': 'cmake',
    })) {
      const version = run(binary, tool === 'go' ? ['version'] : tool === 'actionlint' ? ['-version'] : ['--version'])
      assert.ok(version.includes(manifest.tools[tool]), `${tool}: ${version}`)
    }

    assert.ok(run('mise').includes(manifest.mise))
    run('sh', ['-c', 'for tool in git git-lfs gh codex ssh curl wget zip unzip xz zstd rsync file less tree sqlite3 psql dig ip ping nc ps lsof strace patch diff gcc g++ make pkg-config ninja pdftotext pdftoppm convert ffmpeg; do command -v "$tool" >/dev/null || exit 1; done'])
    assert.equal(run('sh', ['-c', 'printf \'{"ok":true}\' | jq -r .ok']), 'true')
    assert.equal(run('bash', ['-lc', 'fd --version']).includes(manifest.tools.fd), true)
    await writeFile(path.join(directory, 'hello.rs'), 'fn main() { println!("toolkit-rust-ready"); }')
    run('rustc', ['hello.rs', '-o', 'hello'])
    assert.equal(run('./hello', []), 'toolkit-rust-ready')
    run('uv', ['venv', '--python', manifest.tools.python, '.venv'])
    assert.equal(run('.venv/bin/python', ['-c', 'print("toolkit-python-ready")']), 'toolkit-python-ready')
    await writeFile(path.join(directory, 'rust-toolchain.toml'), '[toolchain]\nchannel = "1.98.0"\nprofile = "minimal"\n')
    assert.ok(run('cargo').includes('1.98.0'))
    // Exercise automatic discovery from a packageManager pin distinct from the default.
    await writeFile(path.join(directory, 'package.json'), '{"packageManager":"pnpm@12.3.4"}')
    assert.equal(run('pnpm'), '12.3.4')
    await writeFile(path.join(directory, '.nvmrc'), '22.20.0\n')
    assert.equal(run('node', ['--version']), 'v22.20.0')
    assert.equal(process.versions.node.split('.')[0], '24')
    // Explicit mise config takes precedence, without changing the project files.
    const config = `[tools]\nnode = "${manifest.tools.node}"\npython = "system"\n`
    await writeFile(path.join(directory, 'mise.toml'), config)
    assert.ok((await realpath(run('mise', ['which', 'node']))).includes('/usr/local/share/mise/installs/node/'))
    assert.equal(await readFile(path.join(directory, 'mise.toml'), 'utf8'), config)
    assert.equal(run('pnpm', ['--version'], '/tmp'), manifest.tools.pnpm)
    process.stdout.write('Toolkit passed: every managed tool, system utilities, login shell, Rust compilation, Python venv, project Node/Rust versions, precedence and packageManager pin.\n')
  }
  finally {
    await rm(directory, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
