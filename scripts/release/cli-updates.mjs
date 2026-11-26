import { appendFile } from 'node:fs/promises'
import process from 'node:process'

const stable = /^\d+\.\d+\.\d+$/
const registry = 'https://registry.npmjs.org'

// Coding engines installed from their official npm packages. Builds resolve an
// npm dist-tag or an exact version, then qualify it through its CI adapter.
// `legacy` is the release shipped by images without recorded engine versions.
// See docs/CLI-UPDATES.md.
export const engines = {
  codex: { name: 'Codex', package: '@openai/codex', channel: 'latest' },
  claude: {
    name: 'Claude Code',
    package: '@anthropic-ai/claude-code',
    // Claude Code 2.1.323 puts the bundle over the installed supervisors' limit.
    // Restore `stable` after qualifying a bundle that fits their limits.
    channel: '2.1.311',
    legacy: '2.1.280',
  },
  opencode: {
    name: 'OpenCode',
    package: 'opencode-ai',
    channel: 'latest',
    // The oldest release whose protocol the adapter speaks (opencode::MINIMUM).
    minimum: '1.19.0',
    legacy: '1.19.0',
  },
  // Antigravity has no npm package. Google's updater manifest names only its
  // current release, as one tarball per platform with its SHA-512; earlier
  // releases are not addressable, so it has no channel, legacy or rollback.
  antigravity: {
    name: 'Antigravity',
    manifest: 'https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/linux_amd64.json',
    archive: 'linux-x64/cli_linux_x64.tar.gz',
    // The oldest release the adapter supports (antigravity::MINIMUM).
    minimum: '1.5.14',
  },
}

async function json(url, options = {}) {
  try {
    const response = await fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(15000) })
    if (!response.ok) {
      await response.body?.cancel()
      throw new Error(`HTTP ${response.status}`)
    }

    return await response.json()
  }
  catch (error) {
    throw new Error(`Request to ${url} failed: ${error.message}`)
  }
}

function parts(version) {
  return version.split('.').map(Number)
}

export function newer(left, right) {
  const [a, b] = [parts(left), parts(right)]
  const index = a.findIndex((part, i) => part !== b[i])
  return index >= 0 && a[index] > b[index]
}

async function engineVersion(engine, { request }) {
  const release = await request(`${registry}/${engine.package}/${engine.channel}`)
  const version = release?.version
  if (typeof version !== 'string' || !stable.test(version))
    throw new Error(`${engine.name} ${engine.channel} channel is not a stable release.`)
  if (stable.test(engine.channel) && version !== engine.channel)
    throw new Error(`${engine.name} resolved ${version} instead of pinned version ${engine.channel}.`)
  if (release.deprecated)
    throw new Error(`${engine.name} ${version} on the ${engine.channel} channel is deprecated.`)
  if (engine.minimum && newer(engine.minimum, version))
    throw new Error(`${engine.name} ${version} on the ${engine.channel} channel is older than ${engine.minimum}, the oldest release its adapter supports.`)
  return version
}

// The image installs exactly the tarball the manifest names after checking its
// SHA-512, so the URL is restricted to Google's release bucket and this release.
async function antigravityRelease(engine, { request }) {
  const release = await request(engine.manifest)
  const { version, url, sha512 } = release ?? {}
  if (typeof version !== 'string' || !stable.test(version))
    throw new Error(`${engine.name} manifest does not name a stable release.`)
  if (newer(engine.minimum, version))
    throw new Error(`${engine.name} ${version} is older than ${engine.minimum}, the oldest release its adapter supports.`)
  const prefix = `https://storage.googleapis.com/antigravity-public/antigravity-cli/${version}-`
  const build = typeof url === 'string' && url.startsWith(prefix) && url.endsWith(`/${engine.archive}`)
    ? url.slice(prefix.length, -engine.archive.length - 1)
    : ''
  if (!/^\d+$/.test(build))
    throw new Error(`${engine.name} ${version} manifest does not name its official linux-x64 archive.`)
  if (typeof sha512 !== 'string' || !/^[0-9a-f]{128}$/.test(sha512))
    throw new Error(`${engine.name} ${version} manifest does not carry a SHA-512 digest.`)
  return { version, url, sha512 }
}

export async function buildVersions({ request = json, githubToken } = {}) {
  const githubRelease = request('https://api.github.com/repos/cli/cli/releases/latest', { headers: { Accept: 'application/vnd.github+json', ...(githubToken ? { authorization: `Bearer ${githubToken}` } : {}) } })
  const [codex, claude, opencode, antigravity, github] = await Promise.all([
    engineVersion(engines.codex, { request }),
    engineVersion(engines.claude, { request }),
    engineVersion(engines.opencode, { request }),
    antigravityRelease(engines.antigravity, { request }),
    githubRelease,
  ])
  if (github.draft || github.prerelease)
    throw new Error('GitHub CLI latest release is not stable.')

  const versions = {
    codex,
    gh: github.tag_name?.replace(/^v/, ''),
    claude,
    opencode,
    antigravity: antigravity.version,
  }
  for (const version of Object.values(versions)) {
    if (typeof version !== 'string' || !stable.test(version))
      throw new Error('CLI versions must be stable semantic versions.')
  }

  // Antigravity's archive is not derivable from its version, so the build receives it too.
  return { ...versions, 'antigravity-url': antigravity.url, 'antigravity-sha512': antigravity.sha512 }
}

if (import.meta.main) {
  try {
    if (process.argv[2] !== 'build-versions')
      throw new Error('Usage: cli-updates.mjs build-versions')

    const versions = await buildVersions({ githubToken: process.env.GH_TOKEN })
    if (process.env.GITHUB_OUTPUT)
      await appendFile(process.env.GITHUB_OUTPUT, Object.entries(versions).map(([tool, version]) => `${tool}=${version}\n`).join(''))
    console.log(JSON.stringify(versions))
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
