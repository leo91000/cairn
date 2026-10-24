import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import { appendFile } from 'node:fs/promises'
import process from 'node:process'

// Tools an image records at build time (Dockerfile APP_*_VERSION).
const environment = {
  codex: 'APP_CODEX_VERSION',
  gh: 'APP_GH_VERSION',
  claude: 'APP_CLAUDE_VERSION',
  opencode: 'APP_OPENCODE_VERSION',
}
const manifests = [
  'application/vnd.oci.image.index.v1+json',
  'application/vnd.oci.image.manifest.v1+json',
  'application/vnd.docker.distribution.manifest.list.v2+json',
  'application/vnd.docker.distribution.manifest.v2+json',
].join(', ')

async function request(fetch, url, headers = {}) {
  try {
    const response = await fetch(url, { headers, signal: AbortSignal.timeout(20000) })
    if (!response.ok) {
      await response.body?.cancel()
      throw new Error(`HTTP ${response.status}`)
    }

    return Buffer.from(await response.arrayBuffer())
  }
  catch (error) {
    throw new Error(`Request to ${url} failed: ${error.message}`)
  }
}

// Registry content is addressed by its digest, so a response must hash to it.
async function content(fetch, url, digest, headers) {
  const body = await request(fetch, url, headers)
  if (`sha256:${createHash('sha256').update(body).digest('hex')}` !== digest)
    throw new Error(`Registry content does not match ${digest}`)
  return JSON.parse(body)
}

// The agent tool versions of the installation image production serves, read from
// that immutable image's configuration, so CI can qualify a rollback to it. Images
// built before a tool was resolved do not name it.
export async function servedTools(publicUrl, { fetch = globalThis.fetch } = {}) {
  const release = JSON.parse(await request(fetch, new URL('/install/release', publicUrl)))
  const [, name, digest] = /^ghcr\.io\/([a-z0-9._-]+\/[a-z0-9._-]+)@(sha256:[a-f0-9]{64})$/.exec(release?.image ?? '') ?? []
  if (!digest)
    throw new Error('Production does not report an immutable installation image')
  const { token } = JSON.parse(await request(fetch, `https://ghcr.io/token?scope=repository:${name}:pull`))
  const headers = { Authorization: `Bearer ${token}`, Accept: manifests }
  let manifest = await content(fetch, `https://ghcr.io/v2/${name}/manifests/${digest}`, digest, headers)
  if (manifest.manifests) {
    const platform = manifest.manifests.find(entry => entry.platform?.os === 'linux' && entry.platform?.architecture === 'amd64')
    if (!platform)
      throw new Error('The served image has no linux/amd64 manifest')
    manifest = await content(fetch, `https://ghcr.io/v2/${name}/manifests/${platform.digest}`, platform.digest, headers)
  }

  const image = await content(fetch, `https://ghcr.io/v2/${name}/blobs/${manifest.config.digest}`, manifest.config.digest, { Authorization: headers.Authorization })
  const variables = new Map((image.config?.Env ?? []).map(variable => [variable.slice(0, variable.indexOf('=')), variable.slice(variable.indexOf('=') + 1)]))
  return Object.fromEntries(Object.entries(environment).filter(([, variable]) => variables.get(variable)).map(([tool, variable]) => [tool, variables.get(variable)]))
}

if (import.meta.main) {
  let tools = {}
  try {
    if (!process.env.CAIRN_BEACON_ORIGIN)
      throw new Error('CAIRN_BEACON_ORIGIN is not set')
    tools = await servedTools(process.env.CAIRN_BEACON_ORIGIN)
  }
  catch (error) {
    const message = `The served release's tool versions are unknown: ${error.message}`
    console.error(process.env.GITHUB_ACTIONS ? `::warning title=Served release::${message}` : message)
  }

  if (process.env.GITHUB_OUTPUT)
    await appendFile(process.env.GITHUB_OUTPUT, `opencode=${tools.opencode ?? ''}\n`)
  console.log(JSON.stringify(tools))
}
