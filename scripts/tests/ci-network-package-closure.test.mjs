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

function archiveUrl(snapshotUrl) {
  const url = new URL(snapshotUrl)
  return `https://archive.ubuntu.com/ubuntu${url.pathname.replace(/^\/ubuntu\/\d{8}T\d{6}Z/, '')}`
}

function fixture(options = {}) {
  const directory = mkdtempSync(path.join(tmpdir(), 'cairn-ci-packages-'))
  const manifest = structuredClone(networkPackageManifest)
  const contents = new Map()
  for (const entry of manifest.packages) {
    const bytes = Buffer.from(`archive fixture for ${entry.name}`)
    entry.bytes = bytes.length
    entry.sha256 = createHash('sha256').update(bytes).digest('hex')
    contents.set(entry.url, bytes)
    contents.set(archiveUrl(entry.url), bytes)
  }

  const installed = new Map([['libc6', '2.39'], ...(options.installed ?? [])])
  const calls = []
  const diagnostics = []
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
      expect(init.redirect).toBe('manual')
      return new Response(contents.get(url))
    },
    report: diagnostic => diagnostics.push(diagnostic),
    diagnostics,
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
      const signals = []
      const timeoutBudgets = []
      const result = await acquireNetworkPackages(value.directory, {
        ...value,
        fetchPackage: async (...args) => {
          requests.push(args[0])
          signals.push(args[1].signal)
          return value.fetchPackage(...args)
        },
        createTimeoutSignal: (timeoutMs) => {
          timeoutBudgets.push(timeoutMs)
          return AbortSignal.timeout(timeoutMs)
        },
      })
      expect(requests).toEqual(value.manifest.packages.map(entry => archiveUrl(entry.url)))
      expect(timeoutBudgets).toEqual([30000, 30000, 30000, 30000])
      expect(new Set(signals).size).toBe(4)
      expect(value.diagnostics.map(diagnostic => [diagnostic.package, diagnostic.source, diagnostic.status])).toEqual(
        value.manifest.packages.map(entry => [entry.name, 'archive', 200]),
      )
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

  it.each([
    ['fast-server-error', async () => new Response('unavailable', { status: 503 }), 503, 'http'],
    ['missing-body', async () => new Response(null, { status: 200 }), 200, 'missing-body'],
    ['transport-error', async () => { throw new TypeError('private transport detail') }, null, 'transport'],
  ])('falls back to the pinned snapshot on archive %s within the same package budget', async (_, archiveFailure, expectedStatus, expectedReason) => {
    const value = fixture()
    try {
      const firstEntry = value.manifest.packages[0]
      const firstArchive = archiveUrl(firstEntry.url)
      const requests = []
      const timeoutBudgets = []
      const result = await acquireNetworkPackages(value.directory, {
        ...value,
        createTimeoutSignal: (timeoutMs) => {
          timeoutBudgets.push(timeoutMs)
          return AbortSignal.timeout(timeoutMs)
        },
        fetchPackage: async (url, init) => {
          requests.push({ url, signal: init.signal })
          if (url === firstArchive)
            return archiveFailure()
          return value.fetchPackage(url, init)
        },
      })

      expect(result.packages).toBe(4)
      expect(requests.map(request => request.url)).toEqual([
        firstArchive,
        firstEntry.url,
        ...value.manifest.packages.slice(1).map(entry => archiveUrl(entry.url)),
      ])
      expect(requests[0].signal).toBe(requests[1].signal)
      expect(timeoutBudgets).toEqual([30000, 30000, 30000, 30000])
      expect(value.diagnostics[0]).toMatchObject({
        event: 'source-unavailable',
        package: firstEntry.name,
        source: 'archive',
        status: expectedStatus,
        reason: expectedReason,
      })
      expect(JSON.stringify(value.diagnostics)).not.toContain('private transport detail')
      expect(value.diagnostics.at(-1)).toMatchObject({
        event: 'source-selected',
        package: value.manifest.packages.at(-1).name,
        source: 'archive',
        status: 200,
      })
    }
    finally {
      value.close()
    }
  })

  it('fails after both approved sources are unavailable', async () => {
    const value = fixture()
    try {
      const firstEntry = value.manifest.packages[0]
      const requests = []
      await expect(acquireNetworkPackages(value.directory, {
        ...value,
        fetchPackage: async (url, init) => {
          requests.push(url)
          expect(init.redirect).toBe('manual')
          return new Response('unavailable', { status: url === archiveUrl(firstEntry.url) ? 503 : 502 })
        },
      })).rejects.toThrow('CI_NETWORK_PACKAGES_DOWNLOAD_FAILED')

      expect(requests).toEqual([archiveUrl(firstEntry.url), firstEntry.url])
      expect(value.diagnostics.map(diagnostic => [diagnostic.source, diagnostic.status])).toEqual([['archive', 503], ['snapshot', 502]])
      expect(() => readFileSync(path.join(value.directory, 'manifest.json'))).toThrow()
    }
    finally {
      value.close()
    }
  })

  it('does not start the snapshot request after the shared package deadline expires', async () => {
    const value = fixture()
    try {
      const controller = new AbortController()
      const firstArchive = archiveUrl(value.manifest.packages[0].url)
      const requests = []
      const timeoutBudgets = []
      await expect(acquireNetworkPackages(value.directory, {
        ...value,
        createTimeoutSignal: (timeoutMs) => {
          timeoutBudgets.push(timeoutMs)
          return controller.signal
        },
        fetchPackage: async (url) => {
          requests.push(url)
          controller.abort()
          return new Response('unavailable', { status: 503 })
        },
      })).rejects.toThrow('CI_NETWORK_PACKAGES_DOWNLOAD_FAILED')

      expect(timeoutBudgets).toEqual([30000])
      expect(requests).toEqual([firstArchive])
    }
    finally {
      value.close()
    }
  })

  it('does not follow or fall back from a redirect', async () => {
    const value = fixture()
    try {
      const requests = []
      await expect(acquireNetworkPackages(value.directory, {
        ...value,
        fetchPackage: async (url, init) => {
          requests.push(url)
          expect(init.redirect).toBe('manual')
          return new Response(null, { status: 302, headers: { location: 'https://example.invalid/redirected-package.deb' } })
        },
      })).rejects.toThrow('CI_NETWORK_PACKAGES_REDIRECT_REJECTED')

      expect(requests).toEqual([archiveUrl(value.manifest.packages[0].url)])
      expect(value.diagnostics).toEqual([{
        event: 'source-rejected',
        package: value.manifest.packages[0].name,
        source: 'archive',
        status: 302,
        reason: 'redirect',
      }])
    }
    finally {
      value.close()
    }
  })

  it.each([
    ['bad-hash', 'INTEGRITY_FAILED'],
    ['too-large', 'SIZE_MISMATCH'],
    ['too-small', 'INTEGRITY_FAILED'],
    ['http-error', 'DOWNLOAD_FAILED'],
  ])('rejects acquisition %s without publishing a manifest or falling back', async (scenario, code) => {
    const value = fixture()
    try {
      const requests = []
      const firstEntry = value.manifest.packages[0]
      const fetchPackage = async (url) => {
        requests.push(url)
        if (scenario === 'http-error')
          return new Response('denied', { status: 403 })
        const bytes = scenario === 'too-large'
          ? Buffer.alloc(firstEntry.bytes + 1, 'x')
          : scenario === 'too-small'
            ? Buffer.alloc(firstEntry.bytes - 1, 'x')
            : Buffer.alloc(firstEntry.bytes, 'x')
        return new Response(bytes)
      }

      await expect(acquireNetworkPackages(value.directory, { ...value, fetchPackage })).rejects.toThrow(`CI_NETWORK_PACKAGES_${code}`)

      expect(requests).toEqual([archiveUrl(firstEntry.url)])
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
