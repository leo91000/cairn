import { execFileSync } from 'node:child_process'
import { readFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import os from 'node:os'
import process from 'node:process'
import { chromium } from '@playwright/test'
import { transformWithOxc } from 'vite'

// Run with a fixed reference: node scripts/measure-history-cache.mjs <commit>
const reference = process.argv[2]
if (!reference)
  throw new Error('Supply the reference commit for the 200-event cache')
const file = 'apps/web/src/history-cache.ts'
const sources = {
  reference: execFileSync('git', ['show', `${reference}:${file}`], { encoding: 'utf8' }),
  current: await readFile(file, 'utf8'),
}
const server = createServer((_request, response) => response.end('<!doctype html><title>Cache measurement</title>'))
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
const browser = await chromium.launch({ channel: 'chromium' })
try {
  const page = await browser.newPage()
  await page.goto(`http://127.0.0.1:${server.address().port}`)
  const session = await browser.newBrowserCDPSession()
  const results = {}
  for (const [name, source] of Object.entries(sources)) {
    const { code } = await transformWithOxc(source, file)
    const before = await session.send('SystemInfo.getProcessInfo')
    results[name] = await page.evaluate(async (code) => {
      const moduleUrl = URL.createObjectURL(new Blob([code], { type: 'text/javascript' }))
      const load = () => import(`${moduleUrl}#${crypto.randomUUID()}`)
      let cache = await load()
      await cache.clearHistoryCache()
      const value = {
        version: 1,
        cursor: 1250,
        history: 'v1:measurement:1',
        savedAt: Date.now(),
        oldest: 1,
        hasOlder: false,
        state: { run: null, chat: null, artifacts: [] },
        events: Array.from({ length: 1250 }, (_, index) => ({
          id: index + 1,
          runId: 'measurement',
          createdAt: index + 1,
          type: 'item.completed',
          text: `Output ${index + 1}\n${'Synthetic output line.\n'.repeat(20)}`,
        })),
      }
      const sample = async (action) => {
        const times = []
        for (let i = 0; i < 30; i++) {
          const start = performance.now()
          await action()
          times.push(performance.now() - start)
        }

        return Number(times.sort((a, b) => a - b)[15].toFixed(3))
      }

      const writeMs = await sample(() => cache.writeHistory('measurement', 'chat', value))
      const warmMs = await sample(() => cache.readHistory('measurement', 'chat'))
      const coldMs = await sample(async () => {
        cache = await load() // New JS memory cache; browser/OS filesystem caches remain warm.
        await cache.readHistory('measurement', 'chat')
      })
      const saved = await cache.readHistory('measurement', 'chat')
      await cache.clearHistoryCache()
      URL.revokeObjectURL(moduleUrl)
      return {
        retainedEvents: saved.events.length,
        encodedUtf8Bytes: new TextEncoder().encode(JSON.stringify(saved)).length,
        writeMs,
        warmMs,
        coldMs,
      }
    }, code)
    const after = await session.send('SystemInfo.getProcessInfo')
    const cpuSeconds = info => info.processInfo.filter(item => item.type === 'renderer').reduce((total, item) => total + item.cpuTime, 0)
    results[name].rendererCpuMs = Number(((cpuSeconds(after) - cpuSeconds(before)) * 1000).toFixed(3))
  }

  process.stdout.write(`${JSON.stringify({
    reference,
    node: process.version,
    browser: browser.version(),
    host: {
      platform: os.platform(),
      arch: os.arch(),
      cpu: os.cpus()[0].model,
      logicalCpus: os.cpus().length,
      memoryGiB: Math.round(os.totalmem() / 1024 ** 3),
    },
    workload: '1250 events, 20 synthetic output lines each; 30 writes, 30 warm reads, 30 fresh-module reads; renderer CPU includes module imports and measurement setup',
    results,
  }, null, 2)}\n`)
}
finally {
  await browser.close()
  await new Promise(resolve => server.close(resolve))
}
