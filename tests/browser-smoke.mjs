import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtemp, rm } from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { pathToFileURL } from 'node:url'

async function main() {
  // Runs as the container's normal worker user, without custom library paths.
  const version = process.argv.at(-1)
  assert.match(version, /^\d+\.\d+\.\d+$/)
  const directory = await mkdtemp(path.join(os.tmpdir(), 'leo-browser-smoke-'))
  try {
    process.env.PLAYWRIGHT_BROWSERS_PATH = path.join(directory, 'browsers')
    delete process.env.LD_LIBRARY_PATH
    execFileSync('npm', ['install', '--prefix', directory, '--ignore-scripts', '--no-package-lock', '--no-audit', '--no-fund', `playwright@${version}`], { stdio: 'pipe', timeout: 90000 })
    const modulePath = path.join(directory, 'node_modules/playwright')
    execFileSync(process.execPath, [path.join(modulePath, 'cli.js'), 'install', '--only-shell', 'chromium', 'firefox', 'webkit'], { stdio: 'pipe', timeout: 120000 })
    const engines = await import(pathToFileURL(path.join(modulePath, 'index.mjs')).href)
    for (const name of ['chromium', 'firefox', 'webkit']) {
      const browser = await engines[name].launch()
      try {
        const page = await browser.newPage()
        await page.setContent('<h1>Browser ready</h1>')
        assert.equal(await page.locator('h1').textContent(), 'Browser ready')
        process.stdout.write(`${name}: ${browser.version()} launched and rendered as UID ${process.getuid()}\n`)
      }
      finally {
        await browser.close()
      }
    }
  }
  finally {
    await rm(directory, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
