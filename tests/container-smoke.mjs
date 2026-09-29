import assert from 'node:assert/strict'
import { execFile } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'
import { promisify } from 'node:util'

const exec = promisify(execFile)
const name = `leo-smoke-${randomUUID().slice(0, 8)}`
const volumes = ['data', 'home', 'workspaces'].map(
  suffix => `${name}-${suffix}`,
)

function docker(...args) {
  return exec('docker', ['--context', 'default', ...args], { timeout: args[0] === 'run' ? 180000 : 60000 })
}

async function main() {
  try {
    await docker(
      'run',
      '-d',
      '-e',
      'WORKER_ENABLED=false',
      ...(process.env.GITHUB_TOKEN ? ['-e', 'GITHUB_TOKEN'] : []),
      '--name',
      name,
      '-p',
      '127.0.0.1::4310',
      '-v',
      `${volumes[0]}:/data`,
      '-v',
      `${volumes[1]}:/home/node`,
      '-v',
      `${volumes[2]}:/workspaces`,
      process.argv[2] || 'leo-agent-manager:test',
    )
    const { stdout } = await docker('port', name, '4310/tcp')
    let url = `http://${stdout.trim()}`

    async function ready() {
      for (let i = 0; i < 60; i++) {
        if (
          await fetch(`${url}/health`)
            .then(r => r.ok)
            .catch(() => false)
        ) {
          return
        }

        await setTimeout(200)
      }

      throw new Error('Container did not become healthy')
    }

    await ready()
    const healthDeadline = Date.now() + 15000
    while ((await docker('inspect', '--format', '{{.State.Health.Status}}', name)).stdout.trim() !== 'healthy') {
      assert.ok(Date.now() < healthDeadline, 'Docker health check did not become healthy promptly')
      await setTimeout(200)
    }

    const claudeVersion = (await docker('exec', name, 'claude', '--version')).stdout.trim()
    assert.match(claudeVersion, /^2\.1\.280\b/, 'official Claude Code CLI is installed')
    const health = await fetch(`${url}/health`)
    assert.equal(health.headers.get('cache-control'), 'no-store')
    const version = await health.json()
    assert.equal(typeof version.commit, 'string')
    const expectedCommit = process.env.SMOKE_COMMIT || process.env.GITHUB_SHA
    if (expectedCommit)
      assert.equal(version.commit, expectedCommit)
    assert.equal((await fetch(`${url}/api/tasks`)).status, 401)
    const setup = await fetch(`${url}/api/setup`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({
        setupToken: (await docker('exec', name, 'cat', '/data/setup-token')).stdout.trim(),
        password: 'container-test-password-long',
      }),
    })
    assert.equal(setup.status, 200)
    const session = await setup.json()
    const headers = {
      'cookie': setup.headers.get('set-cookie').split(';')[0],
      'x-csrf-token': session.csrf,
      'content-type': 'application/json',
    }
    const saved = await fetch(`${url}/api/agents`, {
      method: 'POST',
      headers,
      body: JSON.stringify({ name: 'Persistent profile' }),
    })
    assert.equal(saved.status, 200)
    assert.equal(
      (await docker('exec', name, 'id', '-u')).stdout.trim(),
      '1000',
    )
    assert.match(
      (await docker('exec', name, 'pnpm', '--version')).stdout,
      /^12\./,
    )
    const codexVersion = (await docker('exec', name, 'codex', '--version')).stdout.trim()
    assert.equal(codexVersion, `codex-cli ${version.tools.codex}`)
    const ghVersion = (await docker('exec', name, 'gh', '--version')).stdout
    assert.ok(ghVersion.startsWith(`gh version ${version.tools.gh} `))
    if (version.toolkit) {
      const toolkitProbe = await readFile(new URL('./toolkit-probe.mjs', import.meta.url), 'utf8')
      const result = await exec('docker', ['--context', 'default', 'exec', name, '/usr/local/bin/node', '--input-type=module', '-e', toolkitProbe], { timeout: 240000, maxBuffer: 1024 * 1024 })
      process.stdout.write(result.stdout)
    }

    const browserProbe = await readFile(new URL('./browser-smoke.mjs', import.meta.url), 'utf8')
    const packageJson = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'))
    const browsers = await exec('docker', ['--context', 'default', 'exec', name, 'node', '--input-type=module', '-e', browserProbe, packageJson.devDependencies['@playwright/test']], { timeout: 240000, maxBuffer: 1024 * 1024 })
    process.stdout.write(browsers.stdout)
    await docker('restart', name)
    url = `http://${(await docker('port', name, '4310/tcp')).stdout.trim()}`
    await ready()
    const agents = await fetch(`${url}/api/agents`, { headers }).then(r =>
      r.json(),
    )
    assert.equal(agents[0].name, 'Persistent profile')
    assert.equal((await fetch(`${url}/api/session`)).status, 200)
    const page = await fetch(url)
    assert.equal(page.headers.get('cache-control'), 'no-cache')
    assert.match(await page.text(), /Leo/)
    process.stdout.write(
      'Container smoke passed: non-root, CLI tools, Chromium/Firefox/WebKit, auth, Vue build, persistent session and data across restart.\n',
    )
  }
  catch (error) {
    const logs = await docker('logs', '--tail', '50', name).catch(() => null)
    if (logs)
      process.stderr.write(logs.stdout + logs.stderr)
    throw error
  }
  finally {
    await docker('rm', '-f', name).catch(() => {})
    await docker('volume', 'rm', ...volumes).catch(() => {})
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
