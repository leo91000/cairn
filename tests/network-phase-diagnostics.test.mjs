import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { createServer } from 'node:http'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import process from 'node:process'
// This specialist check spawns an intentionally failing Playwright run.
// eslint-disable-next-line test/no-import-node-test
import { test as nodeTest } from 'node:test'
import { fileURLToPath } from 'node:url'
import { chromium } from '@playwright/test'
import { closeNetworkFixture, networkPhaseDiagnostics } from '../scripts/network-phase-diagnostics.mjs'
import { isolateTestCredentials } from '../scripts/test-environment.mjs'

isolateTestCredentials()

nodeTest('phase evidence survives a real Playwright timeout that removes every Page', () => {
  const directory = mkdtempSync(join(tmpdir(), 'cairn-network-timeout-'))
  try {
    const playwright = new URL('../node_modules/@playwright/test/index.mjs', import.meta.url).href
    const diagnostics = new URL('../scripts/network-phase-diagnostics.mjs', import.meta.url).href
    const isolation = new URL('../scripts/test-environment.mjs', import.meta.url).href
    const output = join(directory, 'scenario.json')
    const state = join(directory, 'cleanup.json')
    const config = join(directory, 'playwright.config.mjs')
    writeFileSync(config, `
      import { isolateTestCredentials } from ${JSON.stringify(isolation)}
      isolateTestCredentials()
      export default { testDir: '.', testMatch: 'stall.spec.mjs', workers: 1, retries: 0,
        timeout: 10000, reporter: [['list']], outputDir: ${JSON.stringify(join(directory, 'results'))} }
    `)
    writeFileSync(join(directory, 'stall.spec.mjs'), `
      import { readFileSync, writeFileSync } from 'node:fs'
      import { chromium, test } from ${JSON.stringify(playwright)}
      import { networkPhaseDiagnostics } from ${JSON.stringify(diagnostics)}
      test('controlled pending evaluation', async () => {
        const started = Date.now()
        const phases = networkPhaseDiagnostics(${JSON.stringify(output)})
        const browser = await chromium.launch()
        try {
          const page = await browser.newPage()
          await page.setContent('<h1>Fixture page exists before the timeout</h1>')
          writeFileSync(${JSON.stringify(state)}, JSON.stringify({ beforePages: browser.contexts().flatMap(context => context.pages()).length }))
          phases.enter('message-heading')
          test.setTimeout(Date.now() - started + 1000)
          await page.evaluate(() => new Promise(() => {}))
        }
        finally {
          phases.cleanup('capture-transport')
          const pages = browser.contexts().flatMap(context => context.pages())
          const before = JSON.parse(readFileSync(${JSON.stringify(state)}, 'utf8'))
          writeFileSync(${JSON.stringify(state)}, JSON.stringify({ ...before, afterPages: pages.length }))
          await browser.close()
        }
      })
    `)
    const command = fileURLToPath(new URL('../node_modules/@playwright/test/cli.js', import.meta.url))
    const result = spawnSync(process.execPath, [command, 'test', '--config', config], { encoding: 'utf8', timeout: 30000 })
    assert.ifError(result.error)
    assert.equal(result.status, 1, 'the controlled global timeout must remain a test failure')
    assert.match(result.stdout, /Test timeout of \d+ms exceeded/)
    const report = JSON.parse(readFileSync(join(directory, 'scenario.phases.json'), 'utf8'))
    assert.equal(report.testPhase, 'message-heading')
    assert.equal(report.cleanupPhase, 'capture-transport')
    assert.deepEqual(JSON.parse(readFileSync(state, 'utf8')), { beforePages: 1, afterPages: 0 })
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

for (const failedMarker of ['capture-transport', 'browser-close', 'fixture-close']) {
  nodeTest(`journal failure at ${failedMarker} still disposes the browser and fixture`, async () => {
    const directory = mkdtempSync(join(tmpdir(), 'cairn-network-cleanup-'))
    const journalDirectory = join(directory, 'journal')
    mkdirSync(journalDirectory)
    const phases = networkPhaseDiagnostics(join(journalDirectory, 'scenario.json'))
    const fixture = createServer()
    fixture.unref()
    await new Promise(resolve => fixture.listen(0, '127.0.0.1', resolve))
    let browser
    try {
      browser = await chromium.launch({ timeout: 10000 })
      const page = await browser.newPage()
      browser.on('disconnected', () => phases.event('browser-disconnected'))
      page.on('close', () => phases.event('page-closed'))
      page.context().on('close', () => phases.event('context-closed'))
      phases.enter('browser-page')
      const removeJournal = () => rmSync(journalDirectory, { recursive: true, force: true })
      if (failedMarker === 'capture-transport')
        removeJournal()

      const cleanup = closeNetworkFixture(phases, {
        captureTransport: async () => {
          if (failedMarker === 'browser-close')
            removeJournal()
        },
        browser: {
          close: async () => {
            if (failedMarker === 'fixture-close')
              removeJournal()
            await browser.close()
          },
        },
        fixture: {
          close: () => new Promise((resolve, reject) => fixture.close(error => error ? reject(error) : resolve())),
        },
      })
      await assert.rejects(cleanup, (error) => {
        assert.equal(error.name, 'AggregateError')
        assert.equal(error.message, 'Network qualification cleanup failed')
        assert.equal(error.errors.some(cause => cause.code === 'ENOENT'), true)
        return true
      })
      assert.equal(browser.isConnected(), false, 'journal failure must not retain the actual browser')
      assert.equal(fixture.listening, false, 'journal failure must not retain the actual fixture listener')
    }
    finally {
      await browser?.close().catch(() => {})
      if (fixture.listening)
        await new Promise(resolve => fixture.close(resolve))
      rmSync(directory, { recursive: true, force: true })
    }
  })
}
