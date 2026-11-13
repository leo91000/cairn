import { Buffer } from 'node:buffer'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  chmodSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'

// Immutable nextest release asset; digest also recorded by install-action
// 3a3daafd3b7f23b8f65d9f5ba2db203fb59b79e2/manifests/cargo-nextest.json.
export const nextestArtifact = Object.freeze({
  version: '0.9.149',
  url: 'https://api.github.com/repos/nextest-rs/nextest/releases/assets/660814555',
  bytes: 12121096,
  sha256: '32d1d8c0567b7a80bee2bad8bb09125c846da96bc009b2de11ec08557290c770',
})

function fail(code) {
  throw new Error(`CI_NEXTEST_${code}`)
}

function discard(response) {
  response.body?.cancel().catch(() => {})
}

export async function installNextest(binDirectory, {
  artifact = nextestArtifact,
  githubToken = '',
  fetchAsset = fetch,
  createTimeoutSignal = AbortSignal.timeout,
  run = spawnSync,
  report = () => {},
} = {}) {
  const signal = createTimeoutSignal(120000)
  let stage = 'api'

  function observe(record) {
    try {
      report(record)
    }
    catch {
      // Advisory metadata cannot alter installation or failure behavior.
    }
  }

  function observeResponse(response) {
    const contentType = (response.headers.get('content-type') || '').split(';')[0].trim().toLowerCase()
    const numericHeader = (name) => {
      const value = response.headers.get(name)
      return value && /^\d{1,15}$/.test(value) ? value : null
    }

    observe({
      event: 'nextest-response',
      stage,
      status: response.status,
      contentType: ['application/octet-stream', 'application/json', 'text/html', 'text/plain'].includes(contentType) ? contentType : 'other-or-missing',
      contentBytes: numericHeader('content-length'),
      rateLimitRemaining: numericHeader('x-ratelimit-remaining'),
      rateLimitLimit: numericHeader('x-ratelimit-limit'),
    })
  }

  let response
  try {
    // A credential can accompany only this exact immutable GitHub API request.
    if (githubToken && artifact.url !== nextestArtifact.url)
      fail('AUTH_TARGET_REJECTED')

    const headers = { 'Accept': 'application/octet-stream', 'X-GitHub-Api-Version': '2022-11-28' }
    if (githubToken)
      headers.Authorization = `Bearer ${githubToken}`

    response = await fetchAsset(artifact.url, {
      headers,
      redirect: 'manual',
      signal,
    })
    observeResponse(response)
    if ([301, 302, 303, 307, 308].includes(response.status)) {
      discard(response)
      const location = new URL(response.headers.get('location'))
      if (location.protocol !== 'https:' || location.hostname !== 'release-assets.githubusercontent.com'
        || location.port || location.username || location.password || location.hash) {
        fail('REDIRECT_REJECTED')
      }

      signal.throwIfAborted()
      stage = 'asset'
      response = await fetchAsset(location.href, { redirect: 'manual', signal })
      observeResponse(response)
    }

    if (response.status !== 200 || !response.body
      || response.headers.get('content-type')?.split(';')[0] !== 'application/octet-stream') {
      discard(response)
      fail('DOWNLOAD_FAILED')
    }

    const chunks = []
    let bytes = 0
    for await (const chunk of response.body) {
      bytes += chunk.length
      if (bytes > artifact.bytes)
        fail('SIZE_MISMATCH')
      chunks.push(chunk)
    }

    signal.throwIfAborted()
    stage = 'integrity'
    const contents = Buffer.concat(chunks)
    if (bytes !== artifact.bytes || createHash('sha256').update(contents).digest('hex') !== artifact.sha256)
      fail('INTEGRITY_FAILED')

    mkdirSync(binDirectory, { recursive: true })
    const staging = mkdtempSync(path.join(binDirectory, '.cairn-nextest-'))
    try {
      const archive = path.join(staging, 'nextest.tar.gz')
      const binary = path.join(staging, 'cargo-nextest')
      writeFileSync(archive, contents, { flag: 'wx', mode: 0o600 })

      stage = 'extract'
      const extracted = run('tar', ['-xzf', archive, '--directory', staging, '--no-same-owner', '--no-same-permissions', '--', 'cargo-nextest'], {
        encoding: 'utf8',
        timeout: 10000,
        maxBuffer: 4096,
      })
      if (extracted.error || extracted.status !== 0 || !lstatSync(binary).isFile())
        fail('EXTRACTION_FAILED')
      chmodSync(binary, 0o755)

      stage = 'version'
      const verified = run(binary, ['nextest', '--version'], { encoding: 'utf8', timeout: 5000, maxBuffer: 4096 })
      if (verified.error || verified.status !== 0
        || !verified.stdout?.startsWith(`cargo-nextest ${artifact.version} `)) {
        fail('VERSION_MISMATCH')
      }

      stage = 'publish'
      renameSync(binary, path.join(binDirectory, 'cargo-nextest'))
    }
    finally {
      rmSync(staging, { recursive: true, force: true })
    }

    return { version: artifact.version, bytes, sha256: artifact.sha256 }
  }
  catch (error) {
    const code = error instanceof Error && /^CI_NEXTEST_[A-Z_]{1,64}$/.test(error.message) ? error.message : 'CI_NEXTEST_INSTALL_FAILED'
    observe({ event: 'nextest-failure', stage, code })
    throw new Error(code)
  }
}

if (import.meta.main) {
  try {
    if (process.env.GITHUB_ACTIONS !== 'true' || process.env.RUNNER_ENVIRONMENT !== 'github-hosted'
      || process.env.RUNNER_OS !== 'Linux' || process.env.RUNNER_ARCH !== 'X64') {
      fail('REQUIRES_HOSTED_LINUX_X64')
    }

    const githubToken = process.env.GITHUB_TOKEN
    delete process.env.GITHUB_TOKEN
    if (!githubToken)
      fail('REQUIRES_GITHUB_TOKEN')

    const binDirectory = path.join(process.env.CARGO_HOME || path.join(process.env.HOME, '.cargo'), 'bin')
    console.log(JSON.stringify(await installNextest(binDirectory, {
      githubToken,
      report: record => console.error(JSON.stringify(record)),
    })))
  }
  catch (error) {
    console.error(error.message)
    process.exitCode = 1
  }
}
