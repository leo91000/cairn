import { Buffer } from 'node:buffer'
import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  chmod,
  mkdir,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import process from 'node:process'

import tools from './tools.json' with { type: 'json' }

import { validVersion } from './versions.mjs'

const directory = import.meta.dirname
const settings = '\n[settings]\nidiomatic_version_file_enable_tools = ["node", "pnpm", "npm", "python", "rust", "go", "java"]\nnot_found_auto_install = true\npython.compile = false\n'

function run(command, args, env = process.env) {
  return execFileSync(command, args, {
    env,
    encoding: 'utf8',
    timeout: 600000,
    maxBuffer: 4 * 1024 * 1024,
  }).trim()
}

export function validate(plan) {
  if (!/^\d+\.\d+\.\d+$/.test(plan.mise) || Object.keys(plan.tools).sort().join() !== Object.keys(tools).sort().join())
    throw new Error('Invalid toolkit manifest')
  for (const [tool, version] of Object.entries(plan.tools)) {
    if (!validVersion(tool, version))
      throw new Error('Invalid toolkit version')
  }

  return plan
}

async function get(url) {
  const response = await fetch(url, { signal: AbortSignal.timeout(60000) })
  if (!response.ok)
    throw new Error(`Tool download failed: HTTP ${response.status} from ${new URL(url).hostname}`)
  return response
}

async function installMise(version) {
  const arch = { x64: 'x64', arm64: 'arm64' }[process.arch]
  if (!arch)
    throw new Error('Unsupported toolkit architecture')
  const name = `mise-v${version}-linux-${arch}`
  const base = `https://github.com/jdx/mise/releases/download/v${version}`
  const checksum = (await (await get(`${base}/SHASUMS256.txt`)).text()).split('\n').find(line => line.trim().split(/\s+/)[1]?.replace(/^\.\//, '') === name)?.split(/\s+/)[0]
  const binary = Buffer.from(await (await get(`${base}/${name}`)).arrayBuffer())
  if (createHash('sha256').update(binary).digest('hex') !== checksum)
    throw new Error('Mise checksum mismatch')
  await writeFile('/usr/local/bin/mise', binary)
  await chmod('/usr/local/bin/mise', 0o755)
}

async function main() {
  if (process.argv[2] === 'resolve') {
    const mise = (await (await get('https://mise.jdx.dev/VERSION')).text()).trim()
    const versions = {}
    for (const [tool, request] of Object.entries(tools))
      versions[tool] = run('mise', ['latest', `${tool}@${request}`])
    process.stdout.write(`${JSON.stringify(validate({ mise, tools: versions }))}\n`)
    return
  }

  if (process.argv[2] !== 'install')
    throw new Error('Usage: manage.mjs resolve|install [update-plan.json]')
  const input = JSON.parse(await readFile(process.argv[3] || path.join(directory, 'versions.json'), 'utf8'))
  const plan = validate(input.toolkit || input)
  await installMise(plan.mise)
  const temporary = '/tmp/leo-toolkit-build'
  const env = {
    ...process.env,
    HOME: temporary,
    MISE_DATA_DIR: `${temporary}/mise`,
    MISE_CACHE_DIR: `${temporary}/cache`,
    MISE_GLOBAL_CONFIG_FILE: '/etc/mise/config.toml',
    RUSTUP_HOME: `${directory}/rustup`,
    CARGO_HOME: `${directory}/cargo`,
    MISE_YES: '1',
  }
  await mkdir('/etc/mise', { recursive: true })
  await mkdir(temporary, { recursive: true })
  await writeFile('/etc/mise/config.toml', `[tools]\n${Object.entries(plan.tools).map(([tool, version]) => `${JSON.stringify(tool)} = ${tool === 'rust' ? `{ version = ${JSON.stringify(version)}, profile = "minimal", components = ["rustfmt", "clippy"] }` : JSON.stringify(version)}`).join('\n')}\n${settings}`)
  run('/usr/local/bin/mise', ['install', '--system'], env)
  run('/usr/local/bin/mise', ['reshim', '--system'], env)
  // Core Rust uses rustup homes instead of mise's shared install directory.
  // Runtime homes receive private proxies/settings and read-only toolchain links.
  await writeFile(path.join(directory, 'manifest.json'), JSON.stringify({ ...plan, builtAt: Date.now() }))
  await rm(temporary, { recursive: true, force: true })
}

if (import.meta.main) {
  main().catch((error) => {
    console.error(error.message)
    process.exitCode = 1
  })
}
