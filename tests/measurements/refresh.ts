import type { Browser } from '@playwright/test'
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import {
  access,
  mkdir,
  rename,
  writeFile,
} from 'node:fs/promises'
import {
  cpus,
  freemem,
  networkInterfaces,
  totalmem,
} from 'node:os'
import { join, resolve } from 'node:path'
import process from 'node:process'
import { chromium, expect } from '@playwright/test'
import { build } from 'vite'
import { beaconRelayFixture } from '../e2e/beacon-relay-fixture'

interface RefreshSample {
  published: boolean
  writeMs: number
  totalMs: number
  requests: { route: string, method: string, resource: string }[]
}

interface RefreshMeasurement {
  route: string
  resource: string
  strategy: string
  first: RefreshSample
  warm: RefreshSample[]
}

async function main() {
// Standalone, opt-in experiment: no production services or provider credentials.
  const output = process.argv[2]
  assert(output && process.env.CAIRN_BEACON_TEST_DATABASE_URL, 'Provide an output file and disposable PostgreSQL database')
  for (const prerequisite of ['target/debug/cairn', 'target/debug/cairn-beacon', 'dist/beacon.html', 'dist/index.html']) {
    await access(prerequisite).catch(() => {
      throw new Error(`Missing ${prerequisite}; build the backend binaries and run pnpm build first`)
    })
  }

  const bundleDirectory = resolve('test-results/refresh-measurement')
  await mkdir(bundleDirectory, { recursive: true })
  await build({
    configFile: false,
    define: { 'process.env.NODE_ENV': JSON.stringify('production') },
    build: {
      outDir: bundleDirectory,
      lib: {
        entry: resolve('tests/measurements/refresh-browser.ts'),
        name: 'c4RefreshMeasurementBundle',
        formats: ['iife'],
        fileName: () => 'browser.js',
      },
      minify: false,
    },
  })
  const hostAddress = Object.values(networkInterfaces()).flat().find(address => address?.family === 'IPv4' && !address.internal && !address.address.startsWith('169.254.'))?.address
  assert(hostAddress, 'The real direct fixture requires a non-loopback IPv4 interface')
  const stunUrl = `stun:${hostAddress}:3478`
  const fixture = await beaconRelayFixture(4398)
  let browser: Browser | undefined
  let cookie = ''
  let csrf = ''

  async function request(path: string, body?: object) {
    const response = await fetch(`${fixture.url}/api${path}`, {
      method: body ? 'POST' : 'GET',
      headers: {
        'origin': fixture.url,
        'cookie': cookie,
        'x-csrf-token': csrf,
        'content-type': 'application/json',
      },
      body: body ? JSON.stringify(body) : undefined,
    })
    assert(response.ok, `Fixture request returned ${response.status}`)
    cookie = response.headers.get('set-cookie')?.split(';')[0] || cookie
    return response.json()
  }

  try {
    browser = await chromium.launch({ args: ['--disable-features=WebRtcHideLocalIpsWithMdns'] })
    fixture.beacon(process.env.CAIRN_BEACON_TEST_DATABASE_URL, {
      CAIRN_BEACON_STUN_LISTEN: '0.0.0.0:3478',
      CAIRN_BEACON_STUN_URL: stunUrl,
    })
    await expect.poll(() => fetch(`${fixture.url}/health`).then(r => r.ok).catch(() => false), { timeout: 15000 }).toBe(true)
    const challenge = await request('/account/email-code', { email: `refresh-measurement-${Date.now()}@example.test` })
    await expect.poll(() => fixture.messages.length).toBe(1)
    const session = await request('/account/verify', { challenge: challenge.challenge, code: fixture.messages[0]!.match(/\b\d{8}\b/)![0] })
    csrf = session.csrf
    const claim = await request('/installations/claim-code', {})
    const installationProcess = fixture.start('target/debug/cairn', {
      DATA_DIR: join(fixture.root, 'data'),
      AGENT_HOME: join(fixture.root, 'home'),
      WORKSPACE_ROOTS: fixture.root,
      NODE_ENV: 'test',
      WORKER_ENABLED: 'false',
      HOST: '127.0.0.1',
      PORT: '4399',
      CAIRN_BEACON_ORIGIN: fixture.url,
      CAIRN_INSTALLATION_CLAIM_CODE: claim.code,
      CAIRN_INSTALLATION_NAME: 'Refresh measurement',
      CAIRN_DIRECT_ENABLED: 'true',
      CAIRN_DIRECT_STUN_URLS: stunUrl,
      CODEX_BIN: resolve('tests/fixtures/codex.mjs'),
      CLAUDE_BIN: resolve('tests/fixtures/claude.mjs'),
      OPENCODE_BIN: resolve('tests/fixtures/opencode.mjs'),
      GH_BIN: '/nonexistent/fixture-gh',
    })
    await expect.poll(async () => {
      assert.equal(installationProcess.exitCode, null, 'The installation must remain alive during startup')
      return fetch('http://127.0.0.1:4399/health').then(response => response.ok).catch(() => false)
    }, { timeout: 15000 }).toBe(true)
    let installation = ''
    await expect.poll(async () => {
      const values = await request('/installations')
      installation = values.find((v: { online: boolean }) => v.online)?.id || ''
      return Boolean(installation)
    }).toBe(true)
    const prefix = `/installations/${installation}/api`
    const agent = await request(`${prefix}/agents`, { name: 'Measurement agent' })
    const project = await request(`${prefix}/projects`, { name: 'Measurement project', path: fixture.root, sourceMode: 'local' })
    const task = await request(`${prefix}/tasks`, {
      name: 'Measurement mission',
      agentId: agent.id,
      prompt: 'Fixture only',
      enabled: false,
      worktree: false,
    })
    const cases = [
      {
        kind: 'agents',
        id: agent.id,
        value: { name: agent.name },
        strategy: 'global',
      },
      {
        kind: 'projects',
        id: project.id,
        value: { name: project.name, path: fixture.root, sourceMode: 'local' },
        strategy: 'global',
      },
      {
        kind: 'tasks',
        id: task.id,
        value: { ...task },
        strategy: 'global',
      },
      {
        kind: 'tasks',
        id: task.id,
        value: { ...task },
        strategy: 'task-only-prototype',
      },
    ]
    const measurements: RefreshMeasurement[] = []
    const metadata = {
      measuredAt: new Date().toISOString(),
      source: execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
      workload: {
        agents: (await request(`${prefix}/agents`)).length,
        projects: (await request(`${prefix}/projects`)).length,
        tasks: (await request(`${prefix}/tasks`)).length,
        skills: (await request(`${prefix}/skills`)).length,
        mcps: (await request(`${prefix}/mcps`)).length,
      },
      node: process.version,
      chromium: browser.version(),
      host: {
        cpu: cpus()[0]?.model,
        logicalCpus: cpus().length,
        memoryBytes: totalmem(),
        freeBytes: freemem(),
      },
      interSampleDelayMs: 2000,
      conditions: 'Local VM: HTTP loopback and UDP on its non-loopback interface; debug Rust; fresh installation store/account, reused disposable Beacon DB; fixture providers; established route; first plus 20 warm samples; delay only between trials; state publication/nextTick, excluding editor paint',
    }

    async function persist(complete: boolean) {
      await writeFile(`${output}.tmp`, `${JSON.stringify({ ...metadata, status: complete ? 'complete' : 'partial', measurements }, null, 2)}\n`)
      await rename(`${output}.tmp`, output)
    }

    for (const route of ['relay', 'direct']) {
      const context = await browser.newContext()
      await context.addCookies([{ name: 'cairn_session', value: cookie.split('=').slice(1).join('='), url: fixture.url }])
      const page = await context.newPage()
      if (route === 'relay')
        await page.route('**/direct/authorize', r => r.fulfill({ json: { available: false } }))
      await page.route('**/refresh-measurement', r => r.fulfill({ contentType: 'text/html', body: '<!doctype html><div id="app" data-beacon></div>' }))
      await page.goto(`${fixture.url}/installations/${installation}/refresh-measurement`)
      await page.addScriptTag({ path: join(bundleDirectory, 'browser.js') })
      await page.evaluate(({ installation, csrf }) => (window as any).refreshMeasurement.configure(installation, csrf), { installation, csrf })
      await expect.poll(() => page.evaluate(() => (window as any).refreshMeasurement.route()), { timeout: 30000 }).toBe(route)
      for (const item of cases) {
        const samples: RefreshSample[] = []
        for (let i = 0; i < 21; i++) {
          const sample = await page.evaluate(item => (window as any).refreshMeasurement.sample(item.kind, item.id, { ...item.value, name: `Measurement ${item.kind} ${item.index}` }, item.strategy), { ...item, index: i })
          assert(sample.published, 'The saved name must be visible in workspace state')
          assert.equal(sample.requests.length, item.strategy === 'global' ? 6 : 2)
          assert(sample.requests.every((v: { route: string }) => v.route === route), 'Every delivered request must use the requested route')
          samples.push(sample)
          await new Promise(resolve => setTimeout(resolve, 2000))
        }

        measurements.push({
          route,
          resource: item.kind,
          strategy: item.strategy,
          first: samples[0],
          warm: samples.slice(1),
        })
        await persist(false)
        process.stdout.write(`${JSON.stringify({
          route,
          resource: item.kind,
          strategy: item.strategy,
          samples: samples.length,
        })}\n`)
      }

      await page.evaluate(() => (window as any).refreshMeasurement.stop())
      await context.close()
    }

    await persist(true)
  }
  finally {
    await browser?.close()
    await fixture.close()
  }
}

main().catch((error: unknown) => {
  console.error(error)
  process.exitCode = 1
})
