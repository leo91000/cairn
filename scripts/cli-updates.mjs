import { appendFile } from 'node:fs/promises'
import process from 'node:process'

const stable = /^\d+\.\d+\.\d+$/

async function json(url, options = {}) {
  const response = await fetch(url, { ...options, redirect: 'error', signal: AbortSignal.timeout(15000) })
  if (!response.ok) {
    await response.body?.cancel()
    throw new Error(`Request failed (HTTP ${response.status}).`)
  }

  return response.json()
}

export async function buildVersions({ request = json, githubToken } = {}) {
  const [codex, github] = await Promise.all([
    request('https://registry.npmjs.org/@openai/codex/latest'),
    request('https://api.github.com/repos/cli/cli/releases/latest', { headers: { Accept: 'application/vnd.github+json', ...(githubToken ? { authorization: `Bearer ${githubToken}` } : {}) } }),
  ])
  if (github.draft || github.prerelease)
    throw new Error('GitHub CLI latest release is not stable.')

  const versions = { codex: codex.version, gh: github.tag_name?.replace(/^v/, '') }
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
      await appendFile(process.env.GITHUB_OUTPUT, `codex=${versions.codex}\ngh=${versions.gh}\n`)
    console.log(JSON.stringify(versions))
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
