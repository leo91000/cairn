import { spawnSync } from 'node:child_process'
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { createRequire } from 'node:module'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { describe, expect, it } from 'vitest'
import { parse } from 'yaml'

const require = createRequire(import.meta.url)
const playwright = require.resolve('@playwright/test')
const cli = require.resolve('@playwright/test/cli')
const workflow = parse(readFileSync(new URL('../../.github/workflows/ci.yaml', import.meta.url), 'utf8'))
const shard = workflow.jobs.browser.steps.find(step => step.run?.includes('pnpm test:e2e'))

function executeShard(script, flaky) {
  const directory = mkdtempSync(path.join(os.tmpdir(), 'cairn-browser-gate-'))
  try {
    const bin = path.join(directory, 'bin')
    const home = path.join(directory, 'home')
    mkdirSync(bin)
    mkdirSync(home)
    const config = path.join(directory, 'playwright.config.cjs')
    writeFileSync(config, `module.exports = ${JSON.stringify({
      testDir: directory,
      testMatch: 'case.spec.cjs',
      outputDir: path.join(directory, 'results'),
      reporter: 'json',
      retries: 1,
      projects: [{ name: 'gate' }],
    })}`)
    writeFileSync(path.join(directory, 'case.spec.cjs'), `
const { test, expect } = require(${JSON.stringify(playwright)})
test('controlled qualification case', async ({}, testInfo) => {
  expect(testInfo.retry).toBe(${flaky ? 1 : 0})
})
`)
    // Run the real YAML shell step and pinned CLI, with no browser or application.
    writeFileSync(path.join(bin, 'pnpm'), `#!${process.execPath}
const { spawnSync } = require('node:child_process')
if (process.argv[2] !== 'test:e2e') process.exit(99)
const result = spawnSync(process.execPath, [${JSON.stringify(cli)}, 'test', '--config', ${JSON.stringify(config)}, ...process.argv.slice(3)], { stdio: 'inherit' })
process.exit(result.status ?? 1)
`, { mode: 0o700 })
    const result = spawnSync('/bin/bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
      cwd: directory,
      encoding: 'utf8',
      timeout: 12000,
      env: {
        PATH: `${bin}${path.delimiter}${process.env.PATH}`,
        HOME: home,
        XDG_CONFIG_HOME: path.join(home, '.config'),
        XDG_CACHE_HOME: path.join(home, '.cache'),
        CI: 'true',
        FORCE_COLOR: '0',
        BROWSER_PROJECTS: 'gate',
        BROWSER_WORKERS: '1',
        BROWSER_SHARD: '',
      },
    })
    expect(result.error).toBeUndefined()
    return { status: result.status, report: JSON.parse(result.stdout) }
  }
  finally {
    rmSync(directory, { recursive: true, force: true })
  }
}

describe('browser qualification exit status', () => {
  it('rejects a failed first attempt even when its diagnostic retry passes', () => {
    const permissive = executeShard(shard.run.replaceAll('--fail-on-flaky-tests', ''), true)
    expect(permissive.report.stats).toMatchObject({ expected: 0, unexpected: 0, flaky: 1 })
    expect(permissive.status).toBe(0)

    const strict = executeShard(shard.run, true)
    expect(strict.report.stats).toMatchObject({ expected: 0, unexpected: 0, flaky: 1 })
    expect(strict.status).toBe(1)
  })

  it('accepts a case that passes on its first attempt', () => {
    const result = executeShard(shard.run, false)
    expect(result.report.stats).toMatchObject({ expected: 1, unexpected: 0, flaky: 0 })
    expect(result.status).toBe(0)
  })
})
