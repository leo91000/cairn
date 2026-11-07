import { spawnSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import process from 'node:process'
import { installNetworkPackageClosure } from './ci-network-package-closure.mjs'

const tools = [
  { name: 'ip', package: 'iproute2', version: ['-Version'] },
  { name: 'tc', package: 'iproute2', version: ['-Version'] },
  { name: 'iptables', package: 'iptables', version: ['--version'] },
  { name: 'setpriv', package: 'util-linux', version: ['--version'] },
  { name: 'sysctl', package: 'procps', version: ['--version'] },
  { name: 'kill', package: 'procps', version: ['--version'] },
]
const conntrack = { name: 'conntrack', package: 'conntrack', version: ['--version'] }
const probeOptions = { encoding: 'utf8', maxBuffer: 16384, timeout: 10000 }

export function requiredNetworkTools(mode) {
  if (mode === 'android-direct')
    return [...tools]
  // Every network group runs the common network_bench_test.py, including its
  // real same-server conntrack assertion, even when its own scenarios use NAT.
  if (mode === 'network-bench')
    return [...tools, conntrack]
  throw new Error('CI_NETWORK_TOOLS_INVALID_MODE')
}

function inspect(tool, run) {
  // Inspect the same privileged PATH used by setup and cleanup. A shell builtin
  // kill in the invoking shell is insufficient for `sudo -n kill` during cleanup.
  const found = run('sudo', ['-n', '--', 'sh', '-c', 'set -f; IFS=:; for directory in $PATH; do test -n "$directory" || directory=.; if test -f "$directory/$1" && test -x "$directory/$1"; then exit 0; fi; done; exit 1', 'sh', tool.name], probeOptions)
  if (!found.error && found.status === 1)
    return { tool: tool.name, present: false }
  if (found.error || found.status !== 0)
    throw new Error(`CI_NETWORK_TOOLS_PRESENCE_FAILED:${tool.name}`)

  const version = run('sudo', ['-n', '--', tool.name, ...tool.version], probeOptions)
  if (version.error || version.status !== 0)
    throw new Error(`CI_NETWORK_TOOLS_VERSION_FAILED:${tool.name}`)
  const text = `${version.stdout ?? ''}\n${version.stderr ?? ''}`
  // Fixed fields only: no executable paths, environment, command errors or rules.
  return {
    tool: tool.name,
    present: true,
    version: text.match(/\d+(?:\.\d+)+(?:[-+][\w.]+)?/)?.[0] ?? null,
    ...(tool.name === 'iptables' ? { backend: text.includes('nf_tables') ? 'nf_tables' : text.includes('legacy') ? 'legacy' : 'unknown' } : {}),
  }
}

export function prepareNetworkTools(mode, {
  run = spawnSync,
  report = () => {},
  packageDirectory = 'target/ci-network-tools',
  installClosure = installNetworkPackageClosure,
} = {}) {
  const required = requiredNetworkTools(mode)
  const sudo = run('sudo', ['-n', 'true'], probeOptions)
  if (sudo.error || sudo.status !== 0)
    throw new Error('CI_NETWORK_TOOLS_SUDO_UNAVAILABLE')

  let observed = required.map(tool => inspect(tool, run))
  const missing = required.filter((_, index) => !observed[index].present)
  const packages = [...new Set(missing.map(tool => tool.package))]
  report({
    event: 'CI_NETWORK_TOOLS',
    phase: 'before',
    mode,
    sudoNonInteractive: true,
    tools: observed,
    installPackages: packages,
  })
  if (packages.length) {
    // The supported hosted image supplies the six core tools. Only conntrack
    // has a reviewed package closure; do not repair an arbitrary runner using
    // broad repository updates on this qualification job's critical path.
    if (missing.some(tool => tool.name !== 'conntrack'))
      throw new Error('CI_NETWORK_TOOLS_UNSUPPORTED_HOST')
    report({
      event: 'CI_NETWORK_TOOLS',
      phase: 'offline-install',
      mode,
      tools: observed,
      installPackages: packages,
    })
    const closure = installClosure(packageDirectory, { run })
    report({
      event: 'CI_NETWORK_TOOLS',
      phase: 'offline-installed',
      mode,
      ...closure,
    })

    observed = required.map(tool => inspect(tool, run))
  }

  if (observed.some(tool => !tool.present))
    throw new Error('CI_NETWORK_TOOLS_STILL_MISSING')
  const namespaces = run('sudo', ['-n', '--', 'ip', 'netns', 'list'], probeOptions)
  if (namespaces.error || namespaces.status !== 0)
    throw new Error('CI_NETWORK_TOOLS_NETNS_LIST_FAILED')
  const result = {
    event: 'CI_NETWORK_TOOLS',
    phase: 'ready',
    mode,
    sudoNonInteractive: true,
    netnsList: true,
    tools: observed,
    installPackages: packages,
  }
  report(result)
  return result
}

if (import.meta.main) {
  const mode = process.argv[2]
  const directory = mode === 'network-bench' ? 'test-results/network' : 'test-results/android-direct'
  const report = (value) => {
    mkdirSync(directory, { recursive: true })
    writeFileSync(`${directory}/preflight.json`, `${JSON.stringify(value, null, 2)}\n`)
    console.log(JSON.stringify(value))
  }

  try {
    if (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true')
      throw new Error('CI_NETWORK_TOOLS_REQUIRES_CI')
    prepareNetworkTools(mode, { report })
  }
  catch (error) {
    // Only our fixed codes and whitelisted tool names reach this diagnostic.
    const known = error instanceof Error && /^CI_NETWORK_(?:TOOLS_[A-Z_]+(?::[a-z]+)?|PACKAGES_[A-Z_]+)$/.test(error.message)
    console.error(known ? error.message : 'CI_NETWORK_TOOLS_PREPARATION_FAILED')
    process.exitCode = 1
  }
}
