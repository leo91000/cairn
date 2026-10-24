import { appendFile } from 'node:fs/promises'
import process from 'node:process'

const stable = /^\d+\.\d+\.\d+$/
const registry = 'https://registry.npmjs.org'

// Coding engines installed from their official npm packages. Each build takes the
// version currently published on the vendor's named release channel (an npm
// dist-tag). A series bounds an engine whose adapter only accepts the releases it
// was qualified against: leaving it requires a reviewed change here and in the
// adapter, never a silent upgrade. See docs/CLI-UPDATES.md.
export const engines = {
  codex: { name: 'Codex', package: '@openai/codex', channel: 'latest' },
  // Anthropic's documented stable channel: about a week old, skipping releases
  // with major regressions. Its `latest` channel is not used.
  claude: { name: 'Claude Code', package: '@anthropic-ai/claude-code', channel: 'stable' },
  // crates/installation/src/opencode/mod.rs accepts only this series.
  opencode: {
    name: 'OpenCode',
    package: 'opencode-ai',
    channel: 'latest',
    series: '1.19',
  },
}

async function json(url, options = {}) {
  let response
  try {
    response = await fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(15000) })
  }
  catch (error) {
    throw new Error(`Request to ${url} failed: ${error.message}`)
  }

  if (!response.ok) {
    await response.body?.cancel()
    throw new Error(`Request to ${url} failed (HTTP ${response.status}).`)
  }

  return response.json()
}

function parts(version) {
  return version.split('.').map(Number)
}

function newer(left, right) {
  const [a, b] = [parts(left), parts(right)]
  const index = a.findIndex((part, i) => part !== b[i])
  return index >= 0 && a[index] > b[index]
}

function inSeries(version, series) {
  return !series || version.startsWith(`${series}.`)
}

async function engineVersion(engine, { request, notice }) {
  const release = await request(`${registry}/${engine.package}/${engine.channel}`)
  const version = release?.version
  if (typeof version !== 'string' || !stable.test(version))
    throw new Error(`${engine.name} ${engine.channel} channel is not a stable release.`)
  if (release.deprecated)
    throw new Error(`${engine.name} ${version} on the ${engine.channel} channel is deprecated.`)
  if (inSeries(version, engine.series))
    return version

  // Keep building the newest qualified release until the new series is reviewed.
  const { versions } = await request(`${registry}/${engine.package}`, { headers: { Accept: 'application/vnd.npm.install-v1+json' } })
  const qualified = Object.entries(versions ?? {})
    .filter(([candidate, metadata]) => stable.test(candidate) && inSeries(candidate, engine.series)
      && !metadata?.deprecated && !newer(candidate, version))
    .map(([candidate]) => candidate)
    .reduce((best, candidate) => !best || newer(candidate, best) ? candidate : best, null)
  if (!qualified)
    throw new Error(`${engine.name} ${version} is outside the qualified ${engine.series} series, which has no stable release.`)
  notice(`${engine.name} ${version} on the ${engine.channel} channel is outside the qualified ${engine.series} series; building ${qualified}. Qualify the adapter before widening the series.`)
  return qualified
}

export async function buildVersions({ request = json, githubToken, notice = () => {} } = {}) {
  const githubRelease = request('https://api.github.com/repos/cli/cli/releases/latest', { headers: { Accept: 'application/vnd.github+json', ...(githubToken ? { authorization: `Bearer ${githubToken}` } : {}) } })
  const [codex, claude, opencode, github] = await Promise.all([
    engineVersion(engines.codex, { request, notice }),
    engineVersion(engines.claude, { request, notice }),
    engineVersion(engines.opencode, { request, notice }),
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

    const notice = message => console.error(process.env.GITHUB_ACTIONS ? `::warning title=Agent tool series::${message}` : message)
    const versions = await buildVersions({ githubToken: process.env.GH_TOKEN, notice })
    if (process.env.GITHUB_OUTPUT)
      await appendFile(process.env.GITHUB_OUTPUT, Object.entries(versions).map(([tool, version]) => `${tool}=${version}\n`).join(''))
    console.log(JSON.stringify(versions))
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
