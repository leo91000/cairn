import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import {
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'
import { chromium } from '@playwright/test'
import tailwindcss from '@tailwindcss/vite'
import vue from '@vitejs/plugin-vue'
import { build, preview } from 'vite'
import { isolateTestCredentials } from './test-environment.mjs'

// Production compilation of the real components; no backend or account is used.
// node scripts/measure-chat-rendering.mjs /absolute/path/results.json
isolateTestCredentials()
const output = process.argv[2]
const rawProps = process.argv.includes('--raw-props')
const requestedHistory = process.argv.find(value => value.startsWith('--history='))?.split('=')[1]
const requestedCharacters = process.argv.find(value => value.startsWith('--characters='))?.split('=')[1]
const histories = requestedHistory ? [Number(requestedHistory)] : [40, 400, 2000]
const characterSizes = requestedCharacters ? [Number(requestedCharacters)] : [2000, 20000, 100000]
if (histories.some(value => ![40, 400, 2000].includes(value)) || characterSizes.some(value => ![2000, 20000, 100000].includes(value)))
  throw new Error('Use --history=40|400|2000 and --characters=2000|20000|100000')
if (!output)
  throw new Error('Supply an output JSON path')
const root = fileURLToPath(new URL('..', import.meta.url))
const directory = await mkdtemp(path.join(os.tmpdir(), 'cairn-chat-rendering-'))
let browser
let server
try {
  await build({
    configFile: false,
    root,
    logLevel: 'error',
    plugins: [vue(), tailwindcss()],
    build: {
      outDir: directory,
      rolldownOptions: { input: path.join(root, 'scripts/fixtures/chat-rendering.html') },
    },
  })
  server = await preview({
    configFile: false,
    root,
    build: { outDir: directory },
    preview: { host: '127.0.0.1', port: 0 },
    logLevel: 'error',
  })
  browser = await chromium.launch()
  const report = {
    timestamp: new Date().toISOString(),
    commit: execFileSync('git', ['rev-parse', 'HEAD'], { cwd: root, encoding: 'utf8' }).trim(),
    sourceSha256: Object.fromEntries(await Promise.all([
      'apps/web/src/components/ActivityFeed.vue',
      'apps/web/src/components/Markdown.vue',
      'apps/web/src/highlight-cache.ts',
      'apps/web/src/markdown-html.ts',
      'apps/web/src/message-times.ts',
    ].map(async (file) => {
      const source = await readFile(path.join(root, file)).catch((error) => {
        if (error.code === 'ENOENT')
          return null
        throw error
      })
      return [file, source === null ? null : createHash('sha256').update(source).digest('hex')]
    }))),
    node: process.version,
    browser: browser.version(),
    rawProps,
    histories,
    characterSizes,
    browserIsolation: 'Fresh Chromium process per scenario; warmup remains inside each scenario',
    host: {
      cpu: os.cpus()[0].model,
      logicalCpus: os.cpus().length,
      memoryBytes: os.totalmem(),
      load: os.loadavg(),
      platform: os.platform(),
      arch: os.arch(),
    },
    conditions: 'Headless Chromium, production Vite build, local synthetic props, no network/backend/storage; warmed engine, fresh component mounts; unthrottled CPU. Main-thread update and synchronous layout are timed separately. Scroll is driven at requestAnimationFrame cadence, not touch input.',
    harnessSha256: createHash('sha256').update(await readFile(path.join(root, 'scripts/fixtures/chat-rendering.html'))).update(await readFile(path.join(root, 'scripts/fixtures/chat-rendering.ts'))).update(await readFile(fileURLToPath(import.meta.url))).digest('hex'),
    bootId: (await readFile('/proc/sys/kernel/random/boot_id', 'utf8')).trim(),
    runs: [],
  }
  for (const viewport of [{ width: 1440, height: 900 }, { width: 390, height: 844 }]) {
    let page
    let cdp
    const errors = []

    async function resetBrowser() {
      // Release native Intl/layout allocations as well as the JS heap between cases.
      await browser.close()
      browser = await chromium.launch()
      page = await browser.newPage({ viewport, colorScheme: 'dark' })
      page.on('pageerror', error => errors.push(error.message))
      await page.goto(`${server.resolvedUrls.local[0]}scripts/fixtures/chat-rendering.html`)
      await page.waitForFunction(() => window.chatRendering)
      await page.evaluate(() => document.fonts.ready)
      cdp = await page.context().newCDPSession(page)
      await cdp.send('Performance.enable')
    }

    const run = {
      viewport,
      render: [],
      scroll: [],
      algorithms: [],
    }
    report.runs.push(run)

    async function checkpoint() {
      await writeFile(output, `${JSON.stringify(report, null, 2)}\n`)
    }

    for (const history of histories) {
      for (const characters of characterSizes) {
        await resetBrowser()
        const scenario = {
          history,
          characters,
          workload: 'prose',
          component: 'feed',
          rawProps,
        }
        await cdp.send('HeapProfiler.collectGarbage')
        const before = await cdp.send('Performance.getMetrics')
        const result = await page.evaluate(scenario => window.chatRendering.measure(scenario), scenario)
        const after = await cdp.send('Performance.getMetrics')
        const metric = (data, name) => data.metrics.find(item => item.name === name).value
        result.rendererTaskMsIncludingMountsAndWarmup = 1000 * (metric(after, 'TaskDuration') - metric(before, 'TaskDuration'))
        result.heapBeforeBytes = metric(before, 'JSHeapUsedSize')
        result.heapAfterUnmountBeforeGcBytes = metric(after, 'JSHeapUsedSize')
        run.render.push(result)
        await checkpoint()
        process.stdout.write(`${viewport.width}px ${history} events ${characters} chars: update p50 ${result.update.p50Ms.toFixed(2)}ms; mount p50 ${result.mount.p50Ms.toFixed(2)}ms\n`)
      }

      await resetBrowser()
      run.scroll.push(await page.evaluate(history => window.chatRendering.scroll(history), history))
      run.algorithms.push(await page.evaluate(count => window.chatRendering.algorithms(count), history))
      await checkpoint()
    }

    for (const workload of ['code', 'paragraph']) {
      for (const characters of characterSizes) {
        await resetBrowser()
        run.render.push(await page.evaluate(scenario => window.chatRendering.measure(scenario), {
          history: 0,
          characters,
          workload,
          component: 'markdown',
        }))
        await checkpoint()
      }
    }

    if (errors.length)
      throw new Error(`Browser errors: ${errors.join('; ')}`)
    await page.close()
  }

  await writeFile(output, `${JSON.stringify(report, null, 2)}\n`)
  process.stdout.write(`Saved ${output}\n`)
}
finally {
  await browser?.close()
  await new Promise(resolve => server ? server.httpServer.close(resolve) : resolve())
  await rm(directory, { recursive: true, force: true })
}
