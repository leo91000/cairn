import { spawnSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

const sourceFile = '/etc/apt/sources.list.d/ubuntu.sources'
const dependencyCommand = [
  'apt-get -o Acquire::Retries=0 -o Acquire::https::Timeout=20 -o APT::Update::Error-Mode=any update -qq',
  'apt-get -o Acquire::Retries=0 -o Acquire::https::Timeout=20 install -y --no-install-recommends build-essential curl libclang-dev libssl-dev pkg-config',
].join('\n')

export function canonicalBrowserUbuntuSources(contents) {
  if (/^URIs:[^\n]*\n(?:[ \t]*#[^\n]*\n)*[ \t]+\S/im.test(contents))
    throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
  const seen = new Set()
  const normalized = contents.replace(/^URIs:[ \t]*(\S+)[ \t]*$/gim, (_line, value) => {
    let source
    try {
      source = new URL(value)
    }
    catch {
      throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
    }

    if (!['http:', 'https:'].includes(source.protocol)
      || source.port || source.username || source.password || source.search || source.hash
      || !['/ubuntu', '/ubuntu/'].includes(source.pathname)) {
      throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
    }

    const archive = ['azure.archive.ubuntu.com', 'archive.ubuntu.com'].includes(source.hostname)
    if (!archive && source.hostname !== 'security.ubuntu.com')
      throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
    const host = archive ? 'archive.ubuntu.com' : 'security.ubuntu.com'
    if (seen.has(host))
      throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
    seen.add(host)
    return `URIs: https://${host}/ubuntu/`
  })
  // Refuse source shapes outside the two signed Ubuntu entries in the pinned image.
  const remaining = normalized.replace(/^URIs: https:\/\/(?:archive|security)\.ubuntu\.com\/ubuntu\/$/gm, '')
  if (seen.size !== 2 || /^URIs:/im.test(remaining))
    throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_SOURCES')
  return normalized
}

export function prepareBrowserBuildDependencies({
  run = spawnSync,
  read = readFileSync,
  write = writeFileSync,
  timeoutSeconds = 180,
} = {}) {
  if (!Number.isFinite(timeoutSeconds) || timeoutSeconds <= 0 || timeoutSeconds > 180)
    throw new Error('CI_BROWSER_PACKAGES_INVALID_BUDGET')
  const os = read('/etc/os-release', 'utf8')
  if (!/^ID=ubuntu$/m.test(os) || !/^VERSION_ID="?24\.04"?$/m.test(os))
    throw new Error('CI_BROWSER_PACKAGES_UNSUPPORTED_HOST')
  const normalized = canonicalBrowserUbuntuSources(read(sourceFile, 'utf8'))
  write(sourceFile, normalized)

  // Green acquisition took 17 seconds; include configuration while retaining the
  // job's existing 25-minute budget. GNU timeout owns the apt/dpkg process group.
  const result = run('timeout', ['--signal=TERM', '--kill-after=5s', `${timeoutSeconds}s`, 'bash', '-euc', dependencyCommand], { stdio: 'inherit' })
  if (result.error || result.status !== 0)
    throw new Error(result.status === 124 || result.status === 137 ? 'CI_BROWSER_PACKAGES_TIMEOUT' : 'CI_BROWSER_PACKAGES_FAILED')
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  try {
    prepareBrowserBuildDependencies()
  }
  catch (error) {
    console.error(error instanceof Error && /^CI_BROWSER_PACKAGES_[A-Z_]+$/.test(error.message) ? error.message : 'CI_BROWSER_PACKAGES_FAILED')
    process.exitCode = 1
  }
}
