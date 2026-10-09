import { execFile } from 'node:child_process'
import { readFile, rm, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { promisify } from 'node:util'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { fixture } from '../../tests/fixtures/legacy-helpers.ts'
import { deviceDetails } from '../../tests/fixtures/legacy/server/connections.ts'
import {
  codexArgs,
  redact,
  redactPayload,
  Worker,
} from '../../tests/fixtures/legacy/server/worker.ts'
import { mcpProvider } from '../../tests/fixtures/mcp-provider.ts'

const exec = promisify(execFile)
const binary = path.resolve('tests/fixtures/codex.mjs')
describe('real worker subprocess lifecycle', () => {
  it('redacts configured secrets with JSON escapes while preserving structured output', () => {
    const secret = 'secret-"with\\escapes\nand newlines'
    expect(redactPayload({ nested: [{ text: `Result ${secret}` }] }, [secret])).toEqual({ nested: [{ text: 'Result [redacted]' }] })
  })
  let ctx: Awaited<ReturnType<typeof fixture>>
  beforeEach(async () => {
    ctx = await fixture({ codexBin: binary, concurrency: 2 })
  })
  afterEach(async () => {
    await ctx.dispose()
  })
  const finished = async (id: string) => {
    await expect
      .poll(() => ctx.service.store.run(id)?.status, { timeout: 6000 })
      .not
      .toMatch(/queued|running/)
    return ctx.service.store.run(id)!
  }

  it('passes run-scoped MCP access to the subprocess and redacts and revokes its credential', async () => {
    const provider = await mcpProvider()
    try {
      await ctx.app.listen({ host: '127.0.0.1', port: 0 })
      ctx.service.config.publicUrl = `http://127.0.0.1:${(ctx.app.server.address() as { port: number }).port}`
      await ctx.service.mcps.save({
        name: 'Worker tools',
        url: `${provider.origin}/mcp`,
        auth: 'bearer',
        token: 'fixture-access-token',
        allowPrivateNetwork: true,
      })
      ctx.service.task({ ...ctx.task, prompt: 'fixture:mcp' }, ctx.task.id)
      const run = await ctx.service.enqueue(ctx.task.id)
      await ctx.worker.tick()
      const result = await finished(run.id)
      expect(result.status, result.summary).toBe('succeeded')
      expect(result.summary).toBe('MCP subprocess passed: [redacted]')
      const event = ctx.service.store.events(run.id).find(event => event.text.startsWith('MCP subprocess'))
      expect(event?.payload).toMatchObject({ item: { text: 'MCP subprocess passed: [redacted]' } })
      expect(ctx.service.store.keys('mcp-grant:')).toEqual([])
    }
    finally {
      await provider.close()
    }
  })

  it('executes stdin instructions, streams events, saves usage and redacts credentials', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    const result = await finished(run.id)
    expect(result.status).toBe('succeeded')
    expect(result.summary).toContain('fixture task passed')
    expect(result.sessionId).toBe('fixture-session')
    expect(result.usage).toEqual({ input_tokens: 12, output_tokens: 8 })
    const events = ctx.service.store.events(run.id)
    expect(events.some(event => event.text === 'non-JSON diagnostic')).toBe(
      true,
    )
    expect(JSON.stringify(events)).not.toContain('test-token-value')
    expect(JSON.stringify(events)).toContain('[redacted]')
    expect(events.find(event => event.type === 'thread.started')?.payload).toMatchObject({ type: 'thread.started', thread_id: 'fixture-session' })
    const lastId = events[0].id
    expect(
      ctx.service.store
        .events(run.id, lastId)
        .every(event => event.id > lastId),
    ).toBe(true)
    expect(codexArgs(run, '/tmp/out')).toContain(
      'forced_login_method="chatgpt"',
    )
    expect(codexArgs(run, '/tmp/out')).toEqual(
      expect.arrayContaining(['--dangerously-bypass-approvals-and-sandbox', '--json']),
    )
    expect(codexArgs(run, '/tmp/out')).not.toContain('--sandbox')
    expect(codexArgs(run, '/tmp/out')).not.toContain('-a')
    expect(redact('ghp_abcdefghijklmnop123456 sk-abcdefghijklmnop')).toBe(
      '[redacted] [redacted]',
    )
  })

  it('records nonzero failures and queued cancellation', async () => {
    ctx.service.task({ ...ctx.task, prompt: 'fixture:fail' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    expect((await finished(run.id)).status).toBe('failed')
    const queued = await ctx.service.enqueue(ctx.task.id)
    ctx.worker.cancel(queued.id)
    expect(ctx.service.store.run(queued.id)?.status).toBe('cancelled')
    expect(() => ctx.worker.cancel(queued.id)).toThrow(/finished/)
  })

  it('serializes work in the same project and cancels a running process', async () => {
    ctx.service.task({ ...ctx.task, prompt: 'fixture:hang' }, ctx.task.id)
    const other = ctx.service.task({ ...ctx.task, name: 'Second task' })
    const first = await ctx.service.enqueue(ctx.task.id)
    const second = await ctx.service.enqueue(other.id)
    await ctx.worker.tick()
    await expect
      .poll(() => !!ctx.worker.active.get(first.id)?.child)
      .toBe(true)
    expect(ctx.service.store.run(second.id)?.status).toBe('queued')
    ctx.worker.cancel(first.id)
    expect((await finished(first.id)).status).toBe('cancelled')
    // A terminal run is persisted before asynchronous cleanup releases its
    // project lock. This fixture has no timer to retry a tick that is too early.
    await expect.poll(() => ctx.worker.active.has(first.id)).toBe(false)
    await ctx.worker.tick()
    expect((await finished(second.id)).status).toBe('succeeded')
  })

  it('enforces timeouts and marks crash-interrupted runs without retrying', async () => {
    ctx.service.task({ ...ctx.task, prompt: 'fixture:hang' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    run.snapshot.agent.timeoutMinutes = 0.001
    ctx.service.store.updateRun(run.id, { snapshot: run.snapshot })
    await ctx.worker.tick()
    const result = await finished(run.id)
    expect(result.status).toBe('failed')
    expect(result.summary).toContain('time limit')
    const next = await ctx.service.enqueue(ctx.task.id)
    ctx.service.store.updateRun(next.id, { status: 'running', startedAt: Date.now() })
    new Worker(ctx.service).initialize()
    expect(ctx.service.store.run(next.id)?.status).toBe('interrupted')
    expect(ctx.service.store.active()).toHaveLength(0)
  })

  it('creates a separate Git worktree while preserving a dirty source checkout', async () => {
    await exec('git', ['init', '-b', 'main', ctx.projectPath])
    await writeFile(path.join(ctx.projectPath, 'README.md'), 'original')
    await exec('git', ['-C', ctx.projectPath, 'add', '.'])
    await exec('git', [
      '-C',
      ctx.projectPath,
      '-c',
      'user.name=Test',
      '-c',
      'user.email=test@example.com',
      'commit',
      '-m',
      'Initial',
    ])
    await writeFile(
      path.join(ctx.projectPath, 'README.md'),
      'unrelated dirty changes',
    )
    ctx.service.task({ ...ctx.task, worktree: true }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    const result = await finished(run.id)
    expect(result.status).toBe('succeeded')
    expect(result.workspace).not.toBe(ctx.projectPath)
    expect(
      await readFile(path.join(result.workspace!, 'README.md'), 'utf8'),
    ).toBe('original')
    expect(
      await readFile(path.join(ctx.projectPath, 'README.md'), 'utf8'),
    ).toBe('unrelated dirty changes')
    await writeFile(
      path.join(result.workspace!, 'untracked.txt'),
      'preserve this',
    )
    await expect(ctx.worker.cleanup(run.id)).rejects.toThrow(/untracked/)
    await rm(path.join(result.workspace!, 'untracked.txt'))
    await ctx.worker.cleanup(run.id)
    expect(ctx.service.store.run(run.id)?.workspaceCleanedAt).toBeTypeOf(
      'number',
    )
    await expect(ctx.worker.cleanup(run.id)).rejects.toThrow(
      /no managed worktree/,
    )
  })

  it('parses only intended device credentials and completes a guided login', async () => {
    expect(
      deviceDetails(
        'secret arbitrary text https://hostile.example/ ABCD-12345',
      ),
    ).toEqual({ code: 'ABCD-12345', url: '' })
    const flow = ctx.connections.start('codex')
    await expect.poll(() => flow.code).toBe('ABCD-12345')
    expect(flow.url).toBe('https://auth.openai.com/codex/device')
    await expect.poll(() => flow.state).toBe('complete')
  })
})
