import { randomUUID } from 'node:crypto'
import { writeFile } from 'node:fs/promises'
import { performance } from 'node:perf_hooks'
import process from 'node:process'
import { fixture } from './helpers.ts'

async function main() {
  const ctx = await fixture()
  try {
    const headers = await ctx.login()
    const base = await ctx.service.enqueue(ctx.task.id)
    ctx.service.store.updateRun(base.id, { status: 'succeeded' })
    base.snapshot.task.prompt
      = 'Review dependencies and run the full test suite. '.repeat(350)
    base.summary = 'Validation completed successfully. '.repeat(60)
    const count = 5000
    ctx.service.store.transaction(() => {
      for (let i = 0; i < count; i++) {
        ctx.service.store.addRun({
          ...base,
          id: randomUUID(),
          status: i % 5 ? 'succeeded' : 'failed',
          createdAt: Date.now() - i * 60000,
          startedAt: Date.now() - i * 60000,
          finishedAt: Date.now() - i * 60000,
        })
      }

      for (let i = 0; i < 1000; i++) {
        ctx.service.store.event(
          base.id,
          'progress',
          `Step ${i}: inspected the project`,
        )
      }
    })
    const measurements = []
    for (const url of [
      '/api/runs?limit=40',
      '/api/runs?limit=40&status=failed',
      '/api/overview',
      `/api/runs/${base.id}/events?after=500`,
    ]) {
      const samples: number[] = []
      let bytes = 0
      for (let i = 0; i < 55; i++) {
        const start = performance.now()
        const response = await ctx.app.inject({ url, headers })
        if (response.statusCode !== 200)
          throw new Error(response.body)
        if (i >= 5)
          samples.push(performance.now() - start)
        bytes = response.rawPayload.length
      }

      samples.sort((a, b) => a - b)
      measurements.push({
        url,
        medianMs: +samples[25].toFixed(2),
        p95Ms: +samples[47].toFixed(2),
        bytes,
      })
    }

    const output = {
      node: process.version,
      runs: count + 1,
      events: 1000,
      measuredRequestsPerEndpoint: 50,
      transport: 'Fastify inject, local SQLite WAL',
      measurements,
    }
    if (process.argv[2])
      await writeFile(process.argv[2], `${JSON.stringify(output, null, 2)}\n`)
    process.stdout.write(`${JSON.stringify(output, null, 2)}\n`)
  }
  finally {
    await ctx.dispose()
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
