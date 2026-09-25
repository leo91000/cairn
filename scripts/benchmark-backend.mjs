import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { once } from 'node:events'
import {
  cp,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import os from 'node:os'
import path from 'node:path'
import { performance } from 'node:perf_hooks'
import process from 'node:process'
import { Auth } from '../tests/legacy/server/auth.ts'
import { Store } from '../tests/legacy/server/store.ts'

async function main() {
  if (process.argv[2] === '--node-server') {
    const { buildApp } = await import('../tests/legacy/server/app.ts')
    const config = JSON.parse(await readFile(process.env.LEO_CONFIG, 'utf8'))
    const { app } = await buildApp(config)
    const url = await app.listen({ port: 0, host: '127.0.0.1' })
    process.stdout.write(`Listening on ${url}\n`)
    process.once('SIGTERM', async () => {
      await app.close()
    })
    return
  }

  const directory = await mkdtemp(path.join(os.tmpdir(), 'leo-benchmark-'))
  const template = path.join(directory, 'template')
  const store = new Store(template)
  const session = new Auth(store, 'http://localhost:4310').session()
  const eventRun = randomUUID()
  store.transaction(() => {
    for (let i = 0; i < 1000; i++) {
      const id = i === 0 ? eventRun : randomUUID()
      store.addRun({
        id,
        taskId: `task-${i % 100}`,
        projectId: 'project',
        status: 'succeeded',
        trigger: 'manual',
        createdAt: 1000 + i,
        snapshot: { task: { name: `Task ${i % 100}` }, agent: { name: 'Main agent' } },
        summary: 'A completed task. '.repeat(100),
      })
    }

    const insert = store.db.prepare('INSERT INTO events(run_id,created_at,type,text,payload) VALUES(?,?,?,?,?)')
    for (let i = 0; i < 10000; i++)
      insert.run(eventRun, i, 'item.completed', 'Command completed', JSON.stringify({ type: 'command_execution', command: 'pnpm test', output: 'Test passed. '.repeat(30) }))
  })
  store.close()
  const results = []
  try {
    for (const repeat of [1, 2, 3]) {
      for (const concurrency of [1, 16]) {
        for (const scenario of ['runs', 'events', 'write']) {
          for (const backend of repeat % 2 ? ['node', 'rust'] : ['rust', 'node']) {
            const root = path.join(directory, randomUUID())
            await mkdir(root)
            await cp(template, path.join(root, 'data'), { recursive: true })
            await mkdir(path.join(root, 'home'))
            const file = path.join(root, 'config.json')
            await writeFile(file, JSON.stringify({
              dataDir: path.join(root, 'data'),
              home: path.join(root, 'home'),
              workspaceRoots: [root],
              publicUrl: 'http://localhost:4310',
              host: '127.0.0.1',
              port: 0,
              logger: false,
              workerEnabled: false,
            }))
            const started = performance.now()
            const child = spawn(backend === 'node' ? process.execPath : path.resolve('target/release/leo'), backend === 'node' ? ['--import', 'tsx', import.meta.filename, '--node-server'] : [], {
              env: {
                ...process.env,
                LEO_CONFIG: file,
                LEO_TOOLKIT_DIR: undefined,
                NODE_ENV: 'test',
              },
              stdio: ['ignore', 'pipe', 'inherit'],
            })
            const closed = once(child, 'exit')
            try {
              let output = ''
              const url = await new Promise((resolve, reject) => {
                const timer = setTimeout(() => reject(new Error('Backend startup timed out')), 15000)
                child.stdout.on('data', (chunk) => {
                  output += chunk
                  const match = output.match(/Listening on (http:\/\/\S+)/)
                  if (match) {
                    clearTimeout(timer)
                    resolve(match[1])
                  }
                })
                child.once('exit', () => {
                  clearTimeout(timer)
                  reject(new Error('Backend exited before listening'))
                })
              })
              const startupMs = performance.now() - started
              const route = scenario === 'events' ? `/api/runs/${eventRun}/events?limit=500&after=5000` : scenario === 'runs' ? '/api/runs?limit=100&offset=400' : '/api/agents'
              const headers = {
                'cookie': `leo_session=${session.value}`,
                'x-csrf-token': session.csrf,
                'content-type': 'application/json',
                'accept-encoding': 'identity',
              }
              const request = async () => {
                const response = await fetch(`${url}${route}`, { headers, ...(scenario === 'write' ? { method: 'POST', body: JSON.stringify({ name: 'Benchmark agent' }) } : {}) })
                assert.equal(response.status, 200)
                const body = await response.json()
                if (scenario === 'write')
                  assert.equal(body.name, 'Benchmark agent')
                else
                  assert.equal(body.length, scenario === 'events' ? 500 : 100)
              }

              // Below both production limiters: no bypass and no 429 responses.
              for (let i = 0; i < 20; i++)
                await request()
              const latencies = []
              let next = 0
              const begin = performance.now()
              await Promise.all(Array.from({ length: concurrency }, async () => {
                while (next++ < 240) {
                  const start = performance.now()
                  await request()
                  latencies.push(performance.now() - start)
                }
              }))
              const elapsed = performance.now() - begin
              latencies.sort((a, b) => a - b)
              const status = await readFile(`/proc/${child.pid}/status`, 'utf8')
              const result = {
                backend,
                repeat,
                scenario,
                concurrency,
                requests: latencies.length,
                startupMs,
                requestsPerSecond: latencies.length * 1000 / elapsed,
                p50Ms: latencies[Math.floor(latencies.length * 0.5)],
                p95Ms: latencies[Math.floor(latencies.length * 0.95)],
                rssMiB: Number(status.match(/VmRSS:\s+(\d+)/)[1]) / 1024,
              }
              results.push(result)
              process.stdout.write(`${backend} ${scenario} c${concurrency} r${repeat}: ${result.requestsPerSecond.toFixed(0)} req/s, p95 ${result.p95Ms.toFixed(2)} ms, ${result.rssMiB.toFixed(1)} MiB\n`)
            }
            finally {
              child.kill('SIGTERM')
              await closed
            }

            await rm(root, { recursive: true, force: true })
          }
        }
      }
    }

    await mkdir('docs/performance', { recursive: true })
    await writeFile('docs/performance/rust-backend.json', `${JSON.stringify({
      date: new Date().toISOString(),
      node: process.version,
      platform: `${os.platform()} ${os.release()}`,
      cpu: os.cpus()[0].model,
      httpThreads: Number(process.env.LEO_HTTP_THREADS || 2),
      fixtures: { runs: 1000, events: 10000 },
      results,
    }, null, 2)}\n`)
  }
  finally {
    await rm(directory, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
