import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { describe, expect, it } from 'vitest'
import {
  rollbackTarget,
  rollbackTargets,
  servedTools,
  verifyServed,
} from '../served-tools.mjs'

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

describe('rollback targets', () => {
  it('uses each engine\'s legacy release for images built before it was resolved', () => {
    expect(rollbackTargets({ codex: '0.164.0', gh: '2.103.2' })).toEqual({ claude: '2.1.280', opencode: '1.19.0' })
    expect(rollbackTargets({ claude: '2.1.308', opencode: '1.19.4' })).toEqual({ claude: '2.1.308', opencode: '1.19.4' })
  })

  it('qualifies the served release only when it is known, supported and different', () => {
    expect(rollbackTarget('claude', '2.1.280', '2.1.308')).toEqual({ target: '2.1.280', reason: '' })
    expect(rollbackTarget('opencode', '1.19.4', '1.19.4')).toEqual({ target: null, reason: '' })
    // No version is guessed when production or its image could not be read.
    expect(rollbackTarget('claude', '', '2.1.308')).toMatchObject({ target: null, reason: expect.stringContaining('unknown') })
    // A rollback runs the served image's own adapter.
    expect(rollbackTarget('opencode', '1.18.35', '1.19.4')).toMatchObject({ target: null, reason: expect.stringContaining('older than this adapter\'s minimum 1.19.0') })
  })
})

describe('deploy verification', () => {
  const served = registry({ env: ['APP_CLAUDE_VERSION=2.1.308', 'APP_OPENCODE_VERSION=1.19.4'] })
  const verify = (qualified, resolved) => verifyServed({ qualified, resolved, publicUrl: 'https://cairn.example' }, { fetch: served })

  it('accepts the release the rollback checks used, or this release\'s own engines on a retry', async () => {
    expect(await verify({ claude: '2.1.308', opencode: '1.19.4' }, { claude: '2.1.315', opencode: '1.19.5' })).toEqual({ claude: '2.1.308', opencode: '1.19.4' })
    expect(await verify({ claude: '2.1.280', opencode: '1.19.0' }, { claude: '2.1.308', opencode: '1.19.4' })).toEqual({ claude: '2.1.308', opencode: '1.19.4' })
  })

  it('refuses when another release was deployed since validation, or none was read', async () => {
    await expect(verify({ claude: '2.1.280', opencode: '1.19.0' }, { claude: '2.1.315', opencode: '1.19.5' })).rejects.toThrow('Production now serves')
    await expect(verify(null, { claude: '2.1.308', opencode: '1.19.4' })).rejects.toThrow('could not read the served release')
  })
})

describe('served-tools command', () => {
  it('warns on pull requests and main, but fails a release, when the served release is unknown', () => {
    const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-served-tools-'))
    try {
      const output = path.join(directory, 'outputs')
      for (const [refType, status] of [['branch', 0], ['tag', 1]]) {
        writeFileSync(output, '')
        const { CAIRN_BEACON_ORIGIN: _, ...env } = process.env
        const result = spawnSync(process.execPath, ['scripts/served-tools.mjs'], { encoding: 'utf8', env: { ...env, GITHUB_OUTPUT: output, GITHUB_REF_TYPE: refType }, timeout: 10000 })
        expect(result.status).toBe(status)
        expect(readFileSync(output, 'utf8')).toBe('found=false\nclaude=\nopencode=\n')
      }
    }
    finally {
      rmSync(directory, { recursive: true, force: true })
    }
  })
})
