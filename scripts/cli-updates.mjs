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

export async function buildVersions({ request = json, githubToken } = {}) {
  const githubRelease = request('https://api.github.com/repos/cli/cli/releases/latest', { headers: { Accept: 'application/vnd.github+json', ...(githubToken ? { authorization: `Bearer ${githubToken}` } : {}) } })
  const [codex, claude, opencode, github] = await Promise.all([
    engineVersion(engines.codex, { request }),
    engineVersion(engines.claude, { request }),
    engineVersion(engines.opencode, { request }),
    githubRelease,
  ])
  if (github.draft || github.prerelease)
    throw new Error('GitHub CLI latest release is not stable.')

  const versions = {
    codex,
    gh: github.tag_name?.replace(/^v/, ''),
    claude,
    opencode,
  }
  for (const version of Object.values(versions)) {
    if (typeof version !== 'string' || !stable.test(version))
      throw new Error('CLI versions must be stable semantic versions.')
  }

  return versions
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
