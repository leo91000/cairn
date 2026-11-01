import { Buffer } from 'node:buffer'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { acquireNetworkPackages, installNetworkPackageClosure, networkPackageManifest } from '../ci-network-package-closure.mjs'
import { prepareNetworkTools } from '../prepare-network-tools.mjs'

function fixture(options = {}) {
  const directory = mkdtempSync(path.join(tmpdir(), 'cairn-ci-packages-'))
  const manifest = structuredClone(networkPackageManifest)
  const contents = new Map()
  for (const entry of manifest.packages) {
    const bytes = Buffer.from(`archive fixture for ${entry.name}`)
    entry.bytes = bytes.length
    entry.sha256 = createHash('sha256').update(bytes).digest('hex')
    contents.set(entry.url, bytes)
  }

  const installed = new Map([['libc6', '2.39'], ...(options.installed ?? [])])
  const calls = []
  const run = (command, args) => {
    calls.push({ command, args })
    if (command === 'dpkg-deb') {
      const entry = manifest.packages.find(entry => entry.filename === path.basename(args[1]))
      return { status: 0, stdout: `Package: ${entry.name}\nVersion: ${entry.version}\nArchitecture: ${options.controlArch ?? entry.architecture}\nDepends: ${entry.depends}\n` }
    }

    if (command === 'dpkg-query') {
      const name = args.at(-1).replace(':amd64', '')
      if (!installed.has(name))
        return { status: 1, stdout: '' }
      return { status: 0, stdout: `${options.packageStatus ?? 'installed'}\t${installed.get(name)}\n` }
    }

    if (command === 'dpkg' && args[0] === '--print-architecture')
      return { status: 0, stdout: options.architecture ?? 'amd64' }
    if (command === 'dpkg' && args[0] === '--compare-versions')
      return { status: args[1] === '1' ? 1 : 0 }
    if (args.includes('--install')) {
      if (options.installFails)
        return { status: 1, stderr: 'private package diagnostic' }
      for (const archive of args.slice(args.indexOf('--install') + 1)) {
        const entry = manifest.packages.find(entry => entry.filename === path.basename(archive))
        if (entry.name !== options.staysMissing)
          installed.set(entry.name, entry.version)
      }

      return { status: 0 }
    }

    if (args.includes('--list'))
      return { status: options.loaderFails ? 1 : 0, stdout: ['libmnl.so.0', 'libnetfilter_conntrack.so.3', 'libnfnetlink.so.0', 'libc.so.6'].map(name => `${name} => /usr/lib/${name}`).join('\n') }
    if (args[2] === 'sh')
      return { status: args.at(-1) === 'conntrack' && !installed.has('conntrack') ? 1 : 0 }
    return { status: 0, stdout: 'fixture 1.4.8' }
  }

  return {
    directory,
    manifest,
    contents,
    installed,
    calls,
    run,
    readOS: () => options.os ?? 'ID=ubuntu\nVERSION_ID="24.04"\n',
    fetchPackage: async (url, init) => {
      expect(init.redirect).toBe('error')
      return new Response(contents.get(url))
    },
    close: () => rmSync(directory, { recursive: true, force: true }),
  }
}

describe('pinned network package closure', () => {
  it('refuses acquisition CLI outside disposable CI', () => {
    const cli = fileURLToPath(new URL('../ci-network-package-closure.mjs', import.meta.url))
    const result = spawnSync(process.execPath, [cli, 'acquire', '/unused'], { encoding: 'utf8', env: { CI: '', GITHUB_ACTIONS: '' } })
    expect(result.status).toBe(1)
    expect(result.stdout).toBe('')
    expect(result.stderr.trim()).toBe('CI_NETWORK_PACKAGES_REQUIRES_CI')
  })

  it('acquires each pinned archive once, and the real preparer consumes only local packages', async () => {
    const value = fixture({ installed: [['libmnl0', '9.0'], ['libnfnetlink0', '9.0']] })
    try {
      const requests = []
      const result = await acquireNetworkPackages(value.directory, {
        ...value,
        fetchPackage: async (...args) => {
          requests.push(args[0])
          return value.fetchPackage(...args)
        },
      })
      expect(requests).toEqual(value.manifest.packages.map(entry => entry.url))
      expect(result.packages).toBe(4)
      expect(readFileSync(path.join(value.directory, 'manifest.json'), 'utf8')).toBe(`${JSON.stringify(value.manifest)}\n`)
      const reports = []
      const prepared = prepareNetworkTools('network-bench', {
        run: value.run,
        packageDirectory: value.directory,
        report: value => reports.push(value),
        installClosure: directory => installNetworkPackageClosure(directory, value),
      })
      expect(prepared.phase).toBe('ready')
      const install = value.calls.find(call => call.args.includes('--install'))
      expect(install.args.slice(4).map(archive => path.basename(archive))).toEqual(value.manifest.packages.filter(entry => ['libnetfilter-conntrack3', 'conntrack'].includes(entry.name)).map(entry => entry.filename))
      expect(value.installed.get('libmnl0')).toBe('9.0')
      expect(value.calls.some(call => call.command.includes('apt') || call.args.some(arg => arg.includes('https:')))).toBe(false)
      expect(reports.map(value => value.phase)).toEqual(['before', 'offline-install', 'offline-installed', 'ready'])
    }
    finally {
      value.close()
    }
  })

  it.each(['bad-hash', 'too-large', 'http-error'])('rejects acquisition %s without publishing a manifest', async (scenario) => {
    const value = fixture()
    try {
      const fetchPackage = async () => scenario === 'http-error'
        ? new Response('denied', { status: 403 })
        : new Response(scenario === 'too-large' ? 'x'.repeat(1000) : 'changed')
      await expect(acquireNetworkPackages(value.directory, { ...value, fetchPackage })).rejects.toThrow(/^CI_NETWORK_PACKAGES_/)
      expect(() => readFileSync(path.join(value.directory, 'manifest.json'))).toThrow()
    }
    finally {
      value.close()
    }
  })

  it.each([
    ['wrong-architecture', { architecture: 'arm64' }, 'UNSUPPORTED_HOST'],
    ['wrong-os', { os: 'ID=debian\nVERSION_ID="12"' }, 'UNSUPPORTED_HOST'],
    ['old-libc', { installed: [['libc6', '1']] }, 'UNSUPPORTED_LIBC'],
    ['unconfigured-package', { packageStatus: 'unpacked' }, 'PACKAGE_STATE_INVALID'],
    ['old-library', { installed: [['libmnl0', '1']] }, 'INCOMPATIBLE_INSTALLED_PACKAGE'],
    ['bad-control', { controlArch: 'arm64' }, 'CONTROL_MISMATCH'],
    ['failed-install', { installFails: true }, 'INSTALL_FAILED'],
    ['false-install-success', { staysMissing: 'conntrack' }, 'POST_INSTALL_STATE_INVALID'],
    ['missing-library', { loaderFails: true }, 'ELF_LIBRARIES_FAILED'],
  ])('fails before bench for %s', async (_, options, code) => {
    const value = fixture(options)
    try {
      await acquireNetworkPackages(value.directory, value)
      expect(() => installNetworkPackageClosure(value.directory, value)).toThrow(`CI_NETWORK_PACKAGES_${code}`)
      expect(value.calls.some(call => call.args.includes('apt-get'))).toBe(false)
    }
    finally {
      value.close()
    }
  })

  it.each(['corrupt', 'manifest', 'extra', 'symlink'])('rejects %s artifact contents before installation', async (scenario) => {
    const value = fixture()
    try {
      await acquireNetworkPackages(value.directory, value)
      const archive = path.join(value.directory, value.manifest.packages[0].filename)
      if (scenario === 'corrupt')
        writeFileSync(archive, 'changed')
      if (scenario === 'manifest')
        writeFileSync(path.join(value.directory, 'manifest.json'), '{}')
      if (scenario === 'extra')
        writeFileSync(path.join(value.directory, 'extra.deb'), 'unknown')
      if (scenario === 'symlink') {
        rmSync(archive)
        symlinkSync('manifest.json', archive)
      }

      expect(() => installNetworkPackageClosure(value.directory, value)).toThrow(/^CI_NETWORK_PACKAGES_/)
      expect(value.calls.some(call => call.args.includes('--install'))).toBe(false)
    }
    finally {
      value.close()
    }
  })
})
