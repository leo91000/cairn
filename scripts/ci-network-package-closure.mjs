import { Buffer } from 'node:buffer'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

export const networkPackageManifest = JSON.parse(readFileSync(new URL('./ci-network-packages.json', import.meta.url), 'utf8'))
const probeOptions = { encoding: 'utf8', maxBuffer: 16384, timeout: 10000 }
const packageFetchTimeoutMs = 30000
const digest = bytes => createHash('sha256').update(bytes).digest('hex')
const manifestBytes = manifest => `${JSON.stringify(manifest)}\n`

function fail(code) {
  throw new Error(`CI_NETWORK_PACKAGES_${code}`)
}

function successful(run, command, args, code) {
  const result = run(command, args, probeOptions)
  if (result.error || result.status !== 0)
    fail(code)
  return result.stdout ?? ''
}

function packageSources(entry) {
  let snapshot
  try {
    snapshot = new URL(entry.url)
  }
  catch {
    fail('SOURCE_INVALID')
  }

  const snapshotPool = /^\/ubuntu\/\d{8}T\d{6}Z(\/pool\/.+)$/.exec(snapshot.pathname)
  if (snapshot.protocol !== 'https:'
    || snapshot.hostname !== 'snapshot.ubuntu.com'
    || snapshot.port
    || snapshot.username
    || snapshot.password
    || snapshot.search
    || snapshot.hash
    || !snapshotPool
    || snapshot.pathname.split('/').at(-1) !== entry.filename) {
    fail('SOURCE_INVALID')
  }

  const archive = new URL(`https://archive.ubuntu.com/ubuntu${snapshotPool[1]}`)
  return [
    { kind: 'archive', url: archive.href },
    { kind: 'snapshot', url: snapshot.href },
  ]
}

function reportSource(report, event, entry, source, status, reason) {
  try {
    report({
      event,
      package: entry.name,
      source: source.kind,
      status: Number.isInteger(status) ? status : null,
      ...(reason ? { reason } : {}),
    })
  }
  catch {
    // Diagnostic reporting must never change package acquisition behavior.
  }
}

function isAvailabilityStatus(status) {
  return status === 404 || status === 408 || status === 429 || status >= 500
}

function discardResponseBody(response) {
  try {
    const cancellation = response.body?.cancel()
    cancellation?.catch(() => {})
  }
  catch {
    // The response is already rejected; body cleanup is best effort.
  }
}

// Acquisition happens once in the existing build job. Consumers never consult
// repository indices, mirrors, mutable cache keys, or a different workflow run.
export async function acquireNetworkPackages(directory, {
  manifest = networkPackageManifest,
  fetchPackage = fetch,
  createTimeoutSignal = AbortSignal.timeout,
  report = () => {},
} = {}) {
  mkdirSync(directory, { recursive: true })
  if (readdirSync(directory).length)
    fail('DESTINATION_NOT_EMPTY')
  for (const entry of manifest.packages) {
    const signal = createTimeoutSignal(packageFetchTimeoutMs)
    const sources = packageSources(entry)
    let acquired = false

    for (let index = 0; index < sources.length; index += 1) {
      const source = sources[index]
      const hasFallback = index < sources.length - 1
      if (signal.aborted)
        fail('DOWNLOAD_FAILED')

      let response
      try {
        response = await fetchPackage(source.url, { redirect: 'manual', signal })
      }
      catch {
        reportSource(report, 'source-unavailable', entry, source, null, 'transport')
        if (!hasFallback || signal.aborted)
          fail('DOWNLOAD_FAILED')
        continue
      }

      if (response.status >= 300 && response.status < 400) {
        reportSource(report, 'source-rejected', entry, source, response.status, 'redirect')
        discardResponseBody(response)
        fail('REDIRECT_REJECTED')
      }

      if (!response.ok) {
        const availabilityFailure = isAvailabilityStatus(response.status)
        reportSource(
          report,
          availabilityFailure ? 'source-unavailable' : 'source-rejected',
          entry,
          source,
          response.status,
          'http',
        )
        discardResponseBody(response)
        if (!availabilityFailure || !hasFallback || signal.aborted)
          fail('DOWNLOAD_FAILED')
        continue
      }

      if (!response.body) {
        reportSource(report, 'source-unavailable', entry, source, response.status, 'missing-body')
        if (!hasFallback || signal.aborted)
          fail('DOWNLOAD_FAILED')
        continue
      }

      const chunks = []
      let bytes = 0
      try {
        for await (const chunk of response.body) {
          bytes += chunk.length
          if (bytes > entry.bytes) {
            reportSource(report, 'source-integrity-failed', entry, source, response.status, 'size')
            fail('SIZE_MISMATCH')
          }

          chunks.push(chunk)
        }
      }
      catch (error) {
        if (error instanceof Error && /^CI_NETWORK_PACKAGES_[A-Z_]+$/.test(error.message))
          throw error
        reportSource(report, 'source-unavailable', entry, source, response.status, 'transport')
        if (!hasFallback || signal.aborted)
          fail('DOWNLOAD_FAILED')
        continue
      }

      if (signal.aborted) {
        reportSource(report, 'source-unavailable', entry, source, response.status, 'timeout')
        fail('DOWNLOAD_FAILED')
      }

      const contents = Buffer.concat(chunks)
      if (bytes !== entry.bytes) {
        reportSource(report, 'source-integrity-failed', entry, source, response.status, 'size')
        fail('SIZE_MISMATCH')
      }

      if (digest(contents) !== entry.sha256) {
        reportSource(report, 'source-integrity-failed', entry, source, response.status, 'sha256')
        fail('INTEGRITY_FAILED')
      }

      writeFileSync(path.join(directory, entry.filename), contents, { flag: 'wx', mode: 0o644 })
      reportSource(report, 'source-selected', entry, source, response.status)
      acquired = true
      break
    }

    if (!acquired)
      fail('DOWNLOAD_FAILED')
  }

  writeFileSync(path.join(directory, 'manifest.json'), manifestBytes(manifest), { flag: 'wx', mode: 0o644 })
  return { packages: manifest.packages.length, bytes: manifest.packages.reduce((total, entry) => total + entry.bytes, 0) }
}

function validateArchives(directory, manifest, run) {
  const expected = ['manifest.json', ...manifest.packages.map(entry => entry.filename)].sort()
  if (JSON.stringify(readdirSync(directory).sort()) !== JSON.stringify(expected))
    fail('ARTIFACT_CONTENTS_INVALID')
  for (const filename of expected) {
    if (!lstatSync(path.join(directory, filename)).isFile())
      fail('ARTIFACT_CONTENTS_INVALID')
  }

  if (readFileSync(path.join(directory, 'manifest.json'), 'utf8') !== manifestBytes(manifest))
    fail('MANIFEST_MISMATCH')
  for (const entry of manifest.packages) {
    const archive = path.resolve(directory, entry.filename)
    if (lstatSync(archive).size !== entry.bytes || digest(readFileSync(archive)) !== entry.sha256)
      fail('INTEGRITY_FAILED')
    const control = successful(run, 'dpkg-deb', ['--field', archive], 'CONTROL_FAILED')
    const fields = new Map(control.split('\n').filter(line => /^[A-Z-]+: /i.test(line)).map(line => [line.slice(0, line.indexOf(':')), line.slice(line.indexOf(':') + 2)]))
    for (const [name, expectedValue] of [['Package', entry.name], ['Version', entry.version], ['Architecture', entry.architecture], ['Depends', entry.depends]]) {
      if (fields.get(name) !== expectedValue)
        fail('CONTROL_MISMATCH')
    }
  }
}

function packageState(name, run) {
  // eslint-disable-next-line no-template-curly-in-string -- dpkg-query owns this field interpolation, not JavaScript.
  const result = run('dpkg-query', ['--show', '--showformat=${db:Status-Status}\t${Version}\n', `${name}:amd64`], probeOptions)
  if (!result.error && result.status === 1 && !result.stdout)
    return null
  if (result.error || result.status !== 0)
    fail('PACKAGE_STATE_FAILED')
  const match = /^installed\t(\d[\w.+:~-]{0,100})\n?$/.exec(result.stdout ?? '')
  if (!match)
    fail('PACKAGE_STATE_INVALID')
  return match[1]
}

function versionAtLeast(version, minimum, run) {
  const result = run('dpkg', ['--compare-versions', version, 'ge', minimum], probeOptions)
  if (!result.error && (result.status === 0 || result.status === 1))
    return result.status === 0
  fail('VERSION_COMPARISON_FAILED')
}

export function installNetworkPackageClosure(directory, { manifest = networkPackageManifest, run = spawnSync, readOS = () => readFileSync('/etc/os-release', 'utf8') } = {}) {
  const os = readOS()
  if (!/^ID=ubuntu$/m.test(os) || !/^VERSION_ID="?24\.04"?$/m.test(os))
    fail('UNSUPPORTED_HOST')
  const architecture = successful(run, 'dpkg', ['--print-architecture'], 'ARCHITECTURE_FAILED').trim()
  if (architecture !== manifest.platform.architecture)
    fail('UNSUPPORTED_HOST')
  const libc = packageState('libc6', run)
  if (!libc || !versionAtLeast(libc, manifest.platform.libcMinimum, run))
    fail('UNSUPPORTED_LIBC')
  successful(run, 'sudo', ['-n', '--', 'test', '-x', manifest.platform.loader], 'UNSUPPORTED_LOADER')
  validateArchives(directory, manifest, run)

  const selected = []
  for (const entry of manifest.packages) {
    const installed = packageState(entry.name, run)
    if (installed) {
      if (!versionAtLeast(installed, entry.minimumVersion, run))
        fail('INCOMPATIBLE_INSTALLED_PACKAGE')
      continue
    }

    selected.push(entry)
  }

  if (selected.length) {
    const result = run('sudo', ['-n', '--', 'dpkg', '--install', ...selected.map(entry => path.resolve(directory, entry.filename))], { encoding: 'utf8', maxBuffer: 16384 })
    if (result.error || result.status !== 0)
      fail('INSTALL_FAILED')
  }

  const configuredPackages = []
  for (const entry of manifest.packages) {
    const installed = packageState(entry.name, run)
    if (!installed || !versionAtLeast(installed, entry.minimumVersion, run))
      fail('POST_INSTALL_STATE_INVALID')
    configuredPackages.push({ name: entry.name, version: installed })
  }

  // These are the authenticated distro ELFs, never an arbitrary downloaded
  // executable. Keep the host loader/libc and let the real bench test netlink.
  successful(run, 'sudo', ['-n', '--', manifest.platform.loader, '--verify', '/usr/sbin/conntrack'], 'ELF_VERIFY_FAILED')
  const libraries = successful(run, 'sudo', ['-n', '--', manifest.platform.loader, '--list', '/usr/sbin/conntrack'], 'ELF_LIBRARIES_FAILED')
  for (const name of ['libmnl.so.0', 'libnetfilter_conntrack.so.3', 'libnfnetlink.so.0', 'libc.so.6']) {
    if (!libraries.includes(`${name} => /`) || libraries.includes('not found'))
      fail('ELF_LIBRARIES_FAILED')
  }

  return {
    installedPackages: selected.map(entry => entry.name),
    configuredPackages,
    libcVersion: libc,
    manifestSHA256: digest(manifestBytes(manifest)),
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    if (process.env.CI !== 'true' || process.env.GITHUB_ACTIONS !== 'true')
      fail('REQUIRES_CI')
    if (process.argv[2] !== 'acquire' || process.argv.length !== 4)
      fail('INVALID_ARGUMENTS')
    const result = await acquireNetworkPackages(path.resolve(process.argv[3]), {
      report: diagnostic => console.error(`CI_NETWORK_PACKAGES_SOURCE ${JSON.stringify(diagnostic)}`),
    })
    console.log(JSON.stringify({ event: 'CI_NETWORK_PACKAGES', phase: 'acquired', ...result }))
  }
  catch (error) {
    const known = error instanceof Error && /^CI_NETWORK_PACKAGES_[A-Z_]+$/.test(error.message)
    console.error(known ? error.message : 'CI_NETWORK_PACKAGES_FAILED')
    process.exitCode = 1
  }
}
