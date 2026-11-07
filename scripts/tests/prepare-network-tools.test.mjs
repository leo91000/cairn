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
  versions = {},
  sudoFails = false,
  netnsFails = false,
} = {}) {
  const absent = new Set(missing)
  const calls = []
  const reports = []
  const run = (command, args) => {
    calls.push({ command, args })

    if (args[1] === 'true')
      return { status: sudoFails ? 1 : 0 }
    if (args[2] === 'sh')
      return { status: absent.has(args.at(-1)) ? 1 : 0 }
    if (args.includes('netns'))
      return { status: netnsFails ? 1 : 0 }
    return { status: args[2] === versionFails ? 2 : 0, stdout: versions[args[2]] ?? 'fixture tool version 1.2.3 (nf_tables)' }
  }

  return {
    run,
    calls,
    reports,
    report: value => reports.push(value),
    installClosure: () => {
      if (installFails)
        throw new Error('CI_NETWORK_PACKAGES_INSTALL_FAILED')
      if (!staysMissing)
        absent.clear()
      return { installedPackages: ['conntrack'], manifestSHA256: 'fixture' }
    },
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

  it('retains the complete versions emitted by real iproute2, iptables and conntrack', () => {
    const fixture = host({
      versions: {
        ip: 'ip utility, iproute2-6.1.0, libbpf 1.3.0',
        iptables: 'iptables v1.8.10 (nf_tables)',
        conntrack: 'conntrack v1.4.8 (conntrack-tools)',
      },
    })
    const observed = prepareNetworkTools('network-bench', fixture).tools
    expect(observed.find(tool => tool.tool === 'ip').version).toBe('6.1.0')
    expect(observed.find(tool => tool.tool === 'iptables').version).toBe('1.8.10')
    expect(observed.find(tool => tool.tool === 'conntrack').version).toBe('1.4.8')
  })

  it('uses the reviewed offline closure only for missing conntrack, then reprobes', () => {
    const fixture = host({ missing: ['conntrack'] })
    expect(prepareNetworkTools('network-bench', fixture).installPackages).toEqual(['conntrack'])
    expect(fixture.calls.some(call => call.args.includes('apt-get'))).toBe(false)
    expect(fixture.reports[0].tools.find(tool => tool.tool === 'conntrack').present).toBe(false)
    expect(fixture.reports.find(report => report.phase === 'ready').tools.every(tool => tool.present)).toBe(true)
    expect(fixture.reports.map(report => report.phase)).toEqual(['before', 'offline-install', 'offline-installed', 'ready'])
  })

  it('fails closed instead of broadly provisioning an unsupported runner', () => {
    const fixture = host({ missing: ['ip', 'tc', 'sysctl'] })
    expect(() => prepareNetworkTools('android-direct', fixture)).toThrow('CI_NETWORK_TOOLS_UNSUPPORTED_HOST')
    expect(fixture.calls.some(call => call.args.includes('apt-get') || call.args.includes('dpkg'))).toBe(false)
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
    try {
      for (const tool of requiredNetworkTools('android-direct')) {
        if (tool.name !== 'kill')
          executable(tool.name)
      }

      const run = (_, args, options) => {
        if (args[1] === 'true')
          return { status: 0 }
        const [command, ...parameters] = args.slice(2)
        return spawnSync(command === 'sh' ? '/bin/sh' : join(directory, command), parameters, { ...options, env: { PATH: directory } })
      }

      expect(() => prepareNetworkTools('android-direct', { run })).toThrow('CI_NETWORK_TOOLS_UNSUPPORTED_HOST')
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })

  it.each([
    [{ missing: ['conntrack'], installFails: true }, 'CI_NETWORK_PACKAGES_INSTALL_FAILED'],
    [{ missing: ['conntrack'], staysMissing: true }, 'CI_NETWORK_TOOLS_STILL_MISSING'],
    [{ versionFails: 'ip' }, 'CI_NETWORK_TOOLS_VERSION_FAILED:ip'],
    [{ sudoFails: true }, 'CI_NETWORK_TOOLS_SUDO_UNAVAILABLE'],
    [{ netnsFails: true }, 'CI_NETWORK_TOOLS_NETNS_LIST_FAILED'],
  ])('fails instead of starting tests or retrying after an invalid prerequisite', (options, code) => {
    const fixture = host(options)
    expect(() => prepareNetworkTools('network-bench', fixture)).toThrow(code)
    expect(fixture.reports.some(report => report.phase === 'ready')).toBe(false)
    expect(fixture.calls.some(call => call.args.includes('apt-get'))).toBe(false)
  })
})
