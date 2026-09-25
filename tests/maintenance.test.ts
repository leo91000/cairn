import { randomUUID } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { fixture } from './helpers.ts'

describe('deployment worker lease', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  let headers: { authorization: string }
  const url = '/internal/deployment-lease'
  beforeEach(async () => {
    ctx = await fixture({ codexBin: path.resolve('tests/fixtures/codex.mjs') })
    headers = { authorization: `Bearer ${(await readFile(path.join(ctx.directory, 'data/maintenance-token'), 'utf8')).trim()}` }
  })
  afterEach(async () => {
    await ctx.dispose()
  })
  it('requires its dedicated credential and prevents another lease owner from releasing it', async () => {
    const owner = randomUUID()
    expect((await ctx.app.inject({ method: 'POST', url, payload: { owner } })).statusCode).toBe(401)
    expect((await ctx.app.inject({
      method: 'POST',
      url,
      headers,
      payload: { owner },
    })).json()).toEqual({ paused: true, activeRuns: 0 })
    expect((await ctx.app.inject({
      method: 'DELETE',
      url,
      headers,
      payload: { owner: randomUUID() },
    })).statusCode).toBe(409)
    expect((await ctx.app.inject({
      method: 'DELETE',
      url,
      headers,
      payload: { owner },
    })).json()).toEqual({ paused: false, activeRuns: 0 })
  })
  it('blocks a task even if a tick was already scheduling when the lease arrived', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    let continueSchedule!: () => void
    const scheduled = new Promise<void>((resolve) => {
      continueSchedule = resolve
    })
    vi.spyOn(ctx.service, 'schedule').mockImplementationOnce(() => scheduled)
    const tick = ctx.worker.tick()
    const owner = randomUUID()
    await ctx.app.inject({
      method: 'POST',
      url,
      headers,
      payload: { owner },
    })
    continueSchedule()
    await tick
    expect(ctx.service.store.run(run.id)?.status).toBe('queued')
    expect((await ctx.app.inject('/health')).json().maintenance).toBe(true)
    await ctx.app.inject({
      method: 'DELETE',
      url,
      headers,
      payload: { owner },
    })
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 10000 }).toBe('succeeded')
  })
  it('resumes queued work after an abandoned lease expires', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    ctx.service.store.set('deployment-lease', randomUUID(), Date.now() - 1)
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 10000 }).toBe('succeeded')
  })
})
