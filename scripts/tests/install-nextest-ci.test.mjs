import { Buffer } from 'node:buffer'
import { spawnSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { installNextest, nextestArtifact } from '../install-nextest-ci.mjs'

function fixture(version = nextestArtifact.version) {
  const directory = mkdtempSync(path.join(tmpdir(), 'cairn-nextest-test-'))
  const binary = path.join(directory, 'cargo-nextest')
  writeFileSync(binary, `#!/bin/sh\nprintf 'cargo-nextest ${version} (fixture)\\n'\n`, { mode: 0o755 })
  const archive = path.join(directory, 'fixture.tar.gz')
  const result = spawnSync('tar', ['-czf', archive, '-C', directory, 'cargo-nextest'])
  expect(result.status).toBe(0)
  const contents = readFileSync(archive)
  const artifact = { ...nextestArtifact, bytes: contents.length, sha256: createHash('sha256').update(contents).digest('hex') }
  const bin = path.join(directory, 'bin')
  const requests = []
  const calls = []
  return {
    directory,
    bin,
    artifact,
    contents,
    requests,
    calls,
    fetchAsset: async (url, init) => {
      requests.push({ url, init })
      return new Response(contents, { headers: { 'content-type': 'application/octet-stream' } })
    },
    run: (...args) => {
      calls.push(args)
      return spawnSync(...args)
    },
    close: () => rmSync(directory, { recursive: true, force: true }),
  }
}

describe('fixed nextest acquisition', () => {
  it('retains closed API and asset refusal metadata without signed queries or response bodies', async () => {
    const value = fixture()
    try {
      const records = []
      let requests = 0
      await expect(installNextest(value.bin, {
        ...value,
        report: record => records.push(record),
        fetchAsset: async () => {
          if (requests++ === 0) {
            return new Response(null, {
              status: 302,
              headers: {
                'location': 'https://release-assets.githubusercontent.com/asset?private-signed-query',
                'x-ratelimit-remaining': '53',
                'x-ratelimit-limit': 'private quota header',
              },
            })
          }

          return new Response('private response body', { status: 403, headers: { 'content-type': 'text/html' } })
        },
      })).rejects.toThrow('CI_NEXTEST_DOWNLOAD_FAILED')
      expect(requests).toBe(2)
      expect(records).toEqual([
        {
          event: 'nextest-response',
          stage: 'api',
          status: 302,
          contentType: 'other-or-missing',
          contentBytes: null,
          rateLimitRemaining: '53',
          rateLimitLimit: null,
        },
        {
          event: 'nextest-response',
          stage: 'asset',
          status: 403,
          contentType: 'text/html',
          contentBytes: null,
          rateLimitRemaining: null,
          rateLimitLimit: null,
        },
        { event: 'nextest-failure', stage: 'asset', code: 'CI_NEXTEST_DOWNLOAD_FAILED' },
      ])
      expect(JSON.stringify(records)).not.toContain('private')
      expect(value.calls).toEqual([])
    }
    finally {
      value.close()
    }
  })

  it.each([false, true])('reporter failure cannot change an acquisition outcome (HTTP refusal: %s)', async (refused) => {
    const value = fixture()
    try {
      const attempt = installNextest(value.bin, {
        ...value,
        fetchAsset: refused ? async () => new Response('private body', { status: 504 }) : value.fetchAsset,
        report: () => {
          throw new Error('private diagnostic failure')
        },
      })
      if (refused) {
        await expect(attempt).rejects.toThrow('CI_NEXTEST_DOWNLOAD_FAILED')
        expect(value.calls).toEqual([])
      }
      else {
        await expect(attempt).resolves.toMatchObject({ sha256: value.artifact.sha256 })
      }
    }
    finally {
      value.close()
    }
  })

  it('aborts a real stalled response body under the shared deadline without another request', async () => {
    const value = fixture()
    let requests = 0
    const server = createServer((_request, response) => {
      requests++
      response.writeHead(200, { 'content-type': 'application/octet-stream' })
      response.write(Buffer.from([0]))
    })
    try {
      await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
      await expect(installNextest(value.bin, {
        ...value,
        artifact: { ...value.artifact, url: `http://127.0.0.1:${server.address().port}/asset` },
        fetchAsset: fetch,
        createTimeoutSignal: (budget) => {
          expect(budget).toBe(120000)
          return AbortSignal.timeout(1000)
        },
      })).rejects.toThrow('CI_NEXTEST_INSTALL_FAILED')
      expect(requests).toBe(1)
      expect(value.calls).toEqual([])
      expect(existsSync(value.bin)).toBe(false)
    }
    finally {
      server.closeAllConnections()
      await new Promise(resolve => server.close(resolve))
      value.close()
    }
  })

  it('requests only the pinned official asset and verifies bytes before extraction and publication', async () => {
    const value = fixture()
    try {
      const budgets = []
      const signal = new AbortController().signal
      const result = await installNextest(value.bin, {
        ...value,
        createTimeoutSignal: (budget) => {
          budgets.push(budget)
          return signal
        },
      })
      expect(budgets).toEqual([120000])
      expect(value.requests).toHaveLength(1)
      expect(value.requests[0]).toEqual({
        url: nextestArtifact.url,
        init: {
          headers: { 'Accept': 'application/octet-stream', 'X-GitHub-Api-Version': '2022-11-28' },
          redirect: 'manual',
          signal,
        },
      })
      expect(result).toEqual({ version: nextestArtifact.version, bytes: value.contents.length, sha256: value.artifact.sha256 })
      expect(value.calls.map(([command, args, options]) => ({ command: path.basename(command), args, timeout: options.timeout }))).toEqual([
        { command: 'tar', args: ['-xzf', expect.any(String), '--directory', expect.any(String), '--no-same-owner', '--no-same-permissions', '--', 'cargo-nextest'], timeout: 10000 },
        { command: 'cargo-nextest', args: ['nextest', '--version'], timeout: 5000 },
      ])
      expect(readdirSync(value.bin)).toEqual(['cargo-nextest'])
      expect(spawnSync(path.join(value.bin, 'cargo-nextest'), ['nextest', '--version'], { encoding: 'utf8' }).stdout).toContain('0.9.149')
    }
    finally {
      value.close()
    }
  })

  it('authenticates only the pinned API request and never the signed asset request', async () => {
    const value = fixture()
    try {
      const githubToken = 'fixture-private-token'
      const records = []
      let count = 0
      await installNextest(value.bin, {
        ...value,
        githubToken,
        report: record => records.push(record),
        fetchAsset: async (requested, init) => {
          if (count++ === 0) {
            expect(requested).toBe(nextestArtifact.url)
            expect(init.headers.Authorization).toBe(`Bearer ${githubToken}`)
            return new Response(null, { status: 302, headers: { location: 'https://release-assets.githubusercontent.com/asset?signature=private' } })
          }

          expect(init.headers).toBeUndefined()
          return value.fetchAsset(requested, init)
        },
      })
      expect(count).toBe(2)
      expect(JSON.stringify(records)).not.toContain(githubToken)
    }
    finally {
      value.close()
    }
  })

  it('refuses to send API authentication to an overridden acquisition target', async () => {
    const value = fixture()
    try {
      await expect(installNextest(value.bin, {
        ...value,
        githubToken: 'fixture-private-token',
        artifact: { ...value.artifact, url: 'https://other.example/asset' },
      })).rejects.toThrow('CI_NEXTEST_AUTH_TARGET_REJECTED')
      expect(value.requests).toHaveLength(0)
      expect(value.calls).toHaveLength(0)
    }
    finally {
      value.close()
    }
  })

  it('redacts authenticated API failures without retrying or extracting', async () => {
    const value = fixture()
    try {
      const githubToken = 'fixture-private-token'
      const records = []
      let count = 0
      let authorization
      await expect(installNextest(value.bin, {
        ...value,
        githubToken,
        report: record => records.push(record),
        fetchAsset: async (_requested, init) => {
          count++
          authorization = init.headers.Authorization
          throw new Error(`private failure: ${githubToken}`)
        },
      })).rejects.toThrow('CI_NEXTEST_INSTALL_FAILED')
      expect(count).toBe(1)
      expect(authorization).toBe(`Bearer ${githubToken}`)
      expect(value.calls).toHaveLength(0)
      expect(records).toEqual([{ event: 'nextest-failure', stage: 'api', code: 'CI_NEXTEST_INSTALL_FAILED' }])
    }
    finally {
      value.close()
    }
  })

  it('removes the CLI token from the environment before acquisition or child processes', () => {
    const value = fixture()
    try {
      const githubToken = 'fixture-private-token'
      const receipt = path.join(value.directory, 'environment.json')
      const script = fileURLToPath(new URL('../install-nextest-ci.mjs', import.meta.url))
      const preload = `import { writeFileSync } from 'node:fs';
        const token = process.env.GITHUB_TOKEN;
        globalThis.fetch = async (_url, init) => {
          writeFileSync(${JSON.stringify(receipt)}, JSON.stringify({
            environmentToken: Boolean(process.env.GITHUB_TOKEN),
            authenticated: init.headers.Authorization === 'Bearer ' + token,
          }));
          throw new Error('fixture transport refusal');
        };`
      const result = spawnSync(process.execPath, ['--import', `data:text/javascript,${encodeURIComponent(preload)}`, script], {
        encoding: 'utf8',
        env: {
          GITHUB_ACTIONS: 'true',
          RUNNER_ENVIRONMENT: 'github-hosted',
          RUNNER_OS: 'Linux',
          RUNNER_ARCH: 'X64',
          GITHUB_TOKEN: githubToken,
          CARGO_HOME: value.directory,
        },
      })
      expect(result.status).toBe(1)
      expect(JSON.parse(readFileSync(receipt, 'utf8'))).toEqual({ environmentToken: false, authenticated: true })
      expect(result.stdout).toBe('')
      expect(result.stderr).not.toContain(githubToken)
      expect(result.stderr).toContain('CI_NEXTEST_INSTALL_FAILED')
      expect(existsSync(path.join(value.directory, 'bin'))).toBe(false)
    }
    finally {
      value.close()
    }
  })

  it('requires a step-scoped GitHub token in the hosted installation CLI', () => {
    const script = fileURLToPath(new URL('../install-nextest-ci.mjs', import.meta.url))
    const result = spawnSync(process.execPath, [script], {
      encoding: 'utf8',
      env: {
        GITHUB_ACTIONS: 'true',
        RUNNER_ENVIRONMENT: 'github-hosted',
        RUNNER_OS: 'Linux',
        RUNNER_ARCH: 'X64',
      },
    })
    expect(result.status).toBe(1)
    expect(result.stdout).toBe('')
    expect(result.stderr.trim()).toBe('CI_NEXTEST_REQUIRES_GITHUB_TOKEN')
  })

  it('follows the official release-asset redirect within the same acquisition deadline', async () => {
    const value = fixture()
    try {
      const url = 'https://release-assets.githubusercontent.com/github-production-release-asset/fixture?signature=private'
      const signal = new AbortController().signal
      let count = 0
      await installNextest(value.bin, {
        ...value,
        createTimeoutSignal: () => signal,
        fetchAsset: async (requested, init) => {
          expect(init.signal).toBe(signal)
          expect(init.redirect).toBe('manual')
          if (count++ === 0)
            return new Response(null, { status: 302, headers: { location: url } })
          expect(requested).toBe(url)
          expect(init.headers).toBeUndefined()
          return value.fetchAsset(requested, init)
        },
      })
      expect(count).toBe(2)
    }
    finally {
      value.close()
    }
  })

  it.each(['https://other.example/asset', 'http://release-assets.githubusercontent.com/asset', 'https://user:pass@release-assets.githubusercontent.com/asset', 'https://release-assets.githubusercontent.com:444/asset'])('rejects a redirect to %s without a second request', async (url) => {
    const value = fixture()
    try {
      let count = 0
      await expect(installNextest(value.bin, {
        ...value,
        fetchAsset: async () => {
          count++
          return new Response(null, { status: 302, headers: { location: url } })
        },
      })).rejects.toThrow('CI_NEXTEST_REDIRECT_REJECTED')
      expect(count).toBe(1)
      expect(value.calls).toEqual([])
      expect(existsSync(value.bin)).toBe(false)
    }
    finally {
      value.close()
    }
  })

  it.each(['504', 'transport', 'json', 'short', 'oversize', 'digest', 'timeout', 'body-error', 'second-redirect'])('fails closed on %s without retry, fallback or extraction', async (failure) => {
    const value = fixture()
    try {
      let count = 0
      const controller = new AbortController()
      await expect(installNextest(value.bin, {
        ...value,
        createTimeoutSignal: () => controller.signal,
        fetchAsset: async () => {
          count++
          if (failure === 'transport')
            throw new Error('private signed URL must not escape')
          if (failure === '504')
            return new Response('upstream', { status: 504 })
          if (failure === 'second-redirect')
            return new Response(null, { status: 302, headers: { location: 'https://release-assets.githubusercontent.com/asset' } })
          if (failure === 'json')
            return new Response('{}', { headers: { 'content-type': 'application/json' } })
          if (failure === 'timeout')
            controller.abort()
          if (failure === 'body-error') {
            return new Response(new ReadableStream({
              start(stream) {
                stream.error(new Error('private transport diagnostic'))
              },
            }), { headers: { 'content-type': 'application/octet-stream' } })
          }

          const bytes = failure === 'short'
            ? value.contents.subarray(1)
            : failure === 'oversize'
              ? Buffer.concat([value.contents, Buffer.from('x')])
              : failure === 'digest' ? Buffer.alloc(value.contents.length) : value.contents
          return new Response(bytes, { headers: { 'content-type': 'application/octet-stream' } })
        },
      })).rejects.toThrow(/^CI_NEXTEST_[A-Z_]+$/)
      expect(count).toBe(failure === 'second-redirect' ? 2 : 1)
      expect(value.calls).toEqual([])
      expect(existsSync(value.bin)).toBe(false)
    }
    finally {
      value.close()
    }
  })

  it.each(['extraction', 'symlink', 'version', 'version-exit'])('preserves an existing binary and removes staging on %s failure', async (failure) => {
    const value = fixture(failure === 'version' ? '0.9.148' : nextestArtifact.version)
    try {
      // The installer must publish only after both archive and executable verification.
      mkdirSync(value.bin)
      const installed = path.join(value.bin, 'cargo-nextest')
      writeFileSync(installed, 'existing binary')
      await expect(installNextest(value.bin, {
        ...value,
        run: (command, args, options) => {
          if (failure === 'extraction')
            return { status: 1 }
          if (failure === 'symlink') {
            symlinkSync(installed, path.join(args[3], 'cargo-nextest'))
            return { status: 0 }
          }

          if (failure === 'version-exit' && command !== 'tar')
            return { status: 1, stdout: 'cargo-nextest 0.9.149 (fixture)' }
          return value.run(command, args, options)
        },
      })).rejects.toThrow(/^CI_NEXTEST_[A-Z_]+$/)
      expect(readFileSync(installed, 'utf8')).toBe('existing binary')
      expect(readdirSync(value.bin)).toEqual(['cargo-nextest'])
    }
    finally {
      value.close()
    }
  })

  it('refuses the installation CLI outside disposable Linux x64 CI', () => {
    const script = fileURLToPath(new URL('../install-nextest-ci.mjs', import.meta.url))
    const result = spawnSync(process.execPath, [script], { encoding: 'utf8', env: { GITHUB_ACTIONS: '' } })
    expect(result.status).toBe(1)
    expect(result.stdout).toBe('')
    expect(result.stderr.trim()).toBe('CI_NEXTEST_REQUIRES_HOSTED_LINUX_X64')
  })
})
