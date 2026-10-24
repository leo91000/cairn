import { createHash } from 'node:crypto'
import { describe, expect, it } from 'vitest'
import { servedTools } from '../served-tools.mjs'

const digestOf = body => `sha256:${createHash('sha256').update(body).digest('hex')}`

// Production and a content-addressed registry serving one multi-platform image.
function registry({
  env,
  release,
  tamper = false,
  platforms = ['amd64'],
}) {
  const config = JSON.stringify({ config: { Env: env } })
  const manifest = JSON.stringify({ mediaType: 'application/vnd.oci.image.manifest.v1+json', config: { digest: digestOf(config) } })
  const index = JSON.stringify({
    mediaType: 'application/vnd.oci.image.index.v1+json',
    manifests: platforms.map(architecture => ({ digest: digestOf(manifest), platform: { os: 'linux', architecture } })),
  })
  const image = `ghcr.io/leo91000/cairn@${digestOf(index)}`
  const responses = {
    'https://cairn.example/install/release': JSON.stringify(release ?? { image }),
    'https://ghcr.io/token?scope=repository:leo91000/cairn:pull': JSON.stringify({ token: 'anonymous' }),
    [`https://ghcr.io/v2/leo91000/cairn/manifests/${digestOf(index)}`]: index,
    [`https://ghcr.io/v2/leo91000/cairn/manifests/${digestOf(manifest)}`]: manifest,
    [`https://ghcr.io/v2/leo91000/cairn/blobs/${digestOf(config)}`]: tamper ? config.replace('APP_', 'XAPP_') : config,
  }
  return async (url) => {
    const body = responses[String(url)]
    return body === undefined ? new Response('missing', { status: 404 }) : new Response(body)
  }
}

describe('served release tools', () => {
  it('reads the tool versions recorded in the installation image production serves', async () => {
    const fetch = registry({ env: ['PATH=/usr/bin', 'APP_CODEX_VERSION=0.164.0', 'APP_GH_VERSION=2.103.2', 'APP_CLAUDE_VERSION=2.1.308', 'APP_OPENCODE_VERSION=1.19.4', 'APP_COMMIT=abc'] })
    expect(await servedTools('https://cairn.example', { fetch })).toEqual({
      codex: '0.164.0',
      gh: '2.103.2',
      claude: '2.1.308',
      opencode: '1.19.4',
    })
  })

  it('omits engines an image built before they were resolved does not record', async () => {
    const fetch = registry({ env: ['APP_CODEX_VERSION=0.164.0', 'APP_GH_VERSION=2.103.2'] })
    expect(await servedTools('https://cairn.example', { fetch })).toEqual({ codex: '0.164.0', gh: '2.103.2' })
  })

  it('refuses a mutable image, registry content that misses its digest, or no linux/amd64 image', async () => {
    const env = ['APP_OPENCODE_VERSION=1.19.4']
    await expect(servedTools('https://cairn.example', { fetch: registry({ env, release: { image: 'ghcr.io/leo91000/cairn:latest' } }) }))
      .rejects
      .toThrow('Production does not report an immutable installation image')
    await expect(servedTools('https://cairn.example', { fetch: registry({ env, tamper: true }) })).rejects.toThrow('Registry content does not match')
    await expect(servedTools('https://cairn.example', { fetch: registry({ env, platforms: ['arm64'] }) })).rejects.toThrow('no linux/amd64 manifest')
  })

  it('names the source when production is unavailable', async () => {
    const fetch = async () => new Response('unavailable', { status: 503 })
    await expect(servedTools('https://cairn.example', { fetch })).rejects.toThrow('Request to https://cairn.example/install/release failed: HTTP 503')
  })
})
