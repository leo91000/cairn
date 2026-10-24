import { appendFile } from 'node:fs/promises'
import process from 'node:process'

const stable = /^\d+\.\d+\.\d+$/
const registry = 'https://registry.npmjs.org'

// Coding engines installed from their official npm packages. Each build takes the
// version currently published on the vendor's named release channel (an npm
// dist-tag), like Codex; CI then runs it through its adapter before an image can
// be validated. See docs/CLI-UPDATES.md.
export const engines = {
  codex: { name: 'Codex', package: '@openai/codex', channel: 'latest' },
  // Anthropic's documented stable channel: about a week old, skipping releases
  // with major regressions. Its `latest` channel is not used.
  claude: { name: 'Claude Code', package: '@anthropic-ai/claude-code', channel: 'stable' },
  // The oldest release whose protocol the adapter speaks (opencode::MINIMUM).
  opencode: {
    name: 'OpenCode',
    package: 'opencode-ai',
    channel: 'latest',
    minimum: '1.19.0',
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

function newer(left, right) {
  const [a, b] = [parts(left), parts(right)]
  const index = a.findIndex((part, i) => part !== b[i])
  return index >= 0 && a[index] > b[index]
}

async function engineVersion(engine, { request }) {
  const release = await request(`${registry}/${engine.package}/${engine.channel}`)
  const version = release?.version
  if (typeof version !== 'string' || !stable.test(version))
    throw new Error(`${engine.name} ${engine.channel} channel is not a stable release.`)
  if (release.deprecated)
    throw new Error(`${engine.name} ${version} on the ${engine.channel} channel is deprecated.`)
  if (engine.minimum && newer(engine.minimum, version))
    throw new Error(`${engine.name} ${version} on the ${engine.channel} channel is older than ${engine.minimum}, the oldest release its adapter supports.`)
  return version
}

// The newest stable, non-deprecated release older than `version` that the
// engine's adapter supports, such as the one an installation rolls back to.
export async function previousRelease(tool, version, { request = json } = {}) {
  const engine = engines[tool]
  if (!engine || !stable.test(version ?? ''))
    throw new Error('Usage: cli-updates.mjs previous-release <codex|claude|opencode> <version>')
  const { versions } = await request(`${registry}/${engine.package}`, { headers: { Accept: 'application/vnd.npm.install-v1+json' } })
  return Object.entries(versions ?? {})
    .filter(([candidate, metadata]) => stable.test(candidate) && !metadata?.deprecated && newer(version, candidate)
      && !(engine.minimum && newer(engine.minimum, candidate)))
    .map(([candidate]) => candidate)
    .reduce((best, candidate) => !best || newer(candidate, best) ? candidate : best, null)
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
    if (process.argv[2] === 'previous-release') {
      console.log(await previousRelease(process.argv[3], process.argv[4]) ?? '')
    }
    else if (process.argv[2] === 'build-versions') {
      const versions = await buildVersions({ githubToken: process.env.GH_TOKEN })
      if (process.env.GITHUB_OUTPUT)
        await appendFile(process.env.GITHUB_OUTPUT, Object.entries(versions).map(([tool, version]) => `${tool}=${version}\n`).join(''))
      console.log(JSON.stringify(versions))
    }
    else {
      throw new Error('Usage: cli-updates.mjs build-versions')
    }
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
