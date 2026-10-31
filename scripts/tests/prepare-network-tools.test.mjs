import { spawnSync } from 'node:child_process'
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { prepareNetworkTools, requiredNetworkTools } from '../prepare-network-tools.mjs'

function host({
  missing = [],
  installFails = false,
  staysMissing = false,
  versionFails,
  sudoFails = false,
  netnsFails = false,
} = {}) {
  const absent = new Set(missing)
  const calls = []
  const reports = []
  const run = (command, args) => {
    calls.push({ command, args })
    if (args.includes('apt-get')) {
      if (installFails)
        return { status: 100 }
      if (args.includes('install') && !staysMissing)
        absent.clear()
      return { status: 0 }
    }

    if (args[1] === 'true')
      return { status: sudoFails ? 1 : 0 }
    if (args[2] === 'sh')
      return { status: absent.has(args.at(-1)) ? 1 : 0 }
    if (args.includes('netns'))
      return { status: netnsFails ? 1 : 0 }
    return { status: args[2] === versionFails ? 2 : 0, stdout: 'fixture tool version 1.2.3 (nf_tables)' }
  }

  return {
    run,
    calls,
    reports,
    report: value => reports.push(value),
  }
}

describe('real CI network preparation seam', () => {
  it('refuses CLI invocation outside CI before any privileged command', () => {
    const cli = fileURLToPath(new URL('../prepare-network-tools.mjs', import.meta.url))
    const result = spawnSync(process.execPath, [cli, 'network-bench'], { encoding: 'utf8', env: { CI: '', GITHUB_ACTIONS: '' } })
    expect(result.status).toBe(1)
    expect(result.stdout).toBe('')
    expect(result.stderr.trim()).toBe('CI_NETWORK_TOOLS_REQUIRES_CI')
  })

  it('uses all installed tools, including external cleanup kill, without APT', () => {
    const fixture = host()
    const result = prepareNetworkTools('network-bench', fixture)
    expect(fixture.calls.some(call => call.args.includes('apt-get'))).toBe(false)
    expect(result.tools.map(tool => tool.tool)).toContain('kill')
    expect(result.tools.every(tool => tool.present && tool.version === '1.2.3')).toBe(true)
    expect(result.netnsList).toBe(true)
    expect(fixture.reports.map(report => report.phase)).toEqual(['before', 'ready'])
    expect(fixture.calls.some(call => call.args.join(' ') === '-n -- kill --version')).toBe(true)
  })

  it('installs only deduplicated packages for missing tools, then reprobes', () => {
    const fixture = host({ missing: ['ip', 'tc', 'sysctl'] })
    expect(prepareNetworkTools('android-direct', fixture).installPackages).toEqual(['iproute2', 'procps'])
    expect(fixture.calls.filter(call => call.args.includes('apt-get'))).toEqual([
      { command: 'sudo', args: ['-n', 'apt-get', 'update', '-qq'] },
      { command: 'sudo', args: ['-n', 'apt-get', 'install', '-y', '--no-install-recommends', 'iproute2', 'procps'] },
    ])
    expect(fixture.reports[0].tools.find(tool => tool.tool === 'ip').present).toBe(false)
    expect(fixture.reports.find(report => report.phase === 'ready').tools.every(tool => tool.present)).toBe(true)
    expect(fixture.reports.map(report => report.phase)).toEqual(['before', 'apt-update', 'apt-install', 'ready'])
  })

  it('requires conntrack for every common network suite and never for Android', () => {
    expect(requiredNetworkTools('network-bench').some(tool => tool.name === 'conntrack')).toBe(true)
    expect(requiredNetworkTools('android-direct').some(tool => tool.name === 'conntrack')).toBe(false)
    const fixture = host({ missing: ['conntrack'] })
    expect(prepareNetworkTools('network-bench', fixture).installPackages).toEqual(['conntrack'])
    expect(() => requiredNetworkTools('unknown')).toThrow('CI_NETWORK_TOOLS_INVALID_MODE')
  })

  it('does not mistake shell builtin kill for the executable needed by sudo cleanup', () => {
    const directory = mkdtempSync(join(tmpdir(), 'cairn-network-tools-'))
    const executable = name => writeFileSync(join(directory, name), '#!/bin/sh\necho "fixture 1.2.3"\n', { mode: 0o755 })
    const installed = []
    try {
      for (const tool of requiredNetworkTools('android-direct')) {
        if (tool.name !== 'kill')
          executable(tool.name)
      }

      const run = (_, args, options) => {
        if (args.includes('apt-get')) {
          if (args.includes('install')) {
            installed.push(...args.slice(args.indexOf('--no-install-recommends') + 1))
            executable('kill')
          }

          return { status: 0 }
        }

        if (args[1] === 'true')
          return { status: 0 }
        const [command, ...parameters] = args.slice(2)
        return spawnSync(command === 'sh' ? '/bin/sh' : join(directory, command), parameters, { ...options, env: { PATH: directory } })
      }

      expect(prepareNetworkTools('android-direct', { run }).phase).toBe('ready')
      expect(installed).toEqual(['procps'])
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it.each([
    [{ missing: ['ip'], installFails: true }, 'CI_NETWORK_TOOLS_INSTALL_FAILED'],
    [{ missing: ['ip'], staysMissing: true }, 'CI_NETWORK_TOOLS_STILL_MISSING'],
    [{ versionFails: 'ip' }, 'CI_NETWORK_TOOLS_VERSION_FAILED:ip'],
    [{ sudoFails: true }, 'CI_NETWORK_TOOLS_SUDO_UNAVAILABLE'],
    [{ netnsFails: true }, 'CI_NETWORK_TOOLS_NETNS_LIST_FAILED'],
  ])('fails instead of starting tests or retrying after an invalid prerequisite', (options, code) => {
    const fixture = host(options)
    expect(() => prepareNetworkTools('network-bench', fixture)).toThrow(code)
    expect(fixture.reports.some(report => report.phase === 'ready')).toBe(false)
    expect(fixture.calls.filter(call => call.args.includes('update')).length).toBeLessThanOrEqual(1)
  })
})
