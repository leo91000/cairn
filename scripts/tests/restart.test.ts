import type { ChildProcess } from 'node:child_process'
import { execFile, spawn } from 'node:child_process'
import { readFile, rm, writeFile } from 'node:fs/promises'
import http from 'node:http'
import path from 'node:path'
import process from 'node:process'
import { promisify } from 'node:util'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { accountFixture, credential, limits } from '../../tests/fixtures/codex-account-fixture.ts'
import { fixture } from '../../tests/fixtures/legacy-helpers.ts'
import { prepareExecution } from '../../tests/fixtures/legacy/server/execution.ts'
import { processIdentity } from '../../tests/fixtures/legacy/server/run-recovery.ts'
import { Worker } from '../../tests/fixtures/legacy/server/worker.ts'
import { runnerProvider } from '../../tests/fixtures/runner-provider.ts'

describe('durable conversation recovery', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  const workers: Worker[] = []
  const processes: ChildProcess[] = []
  beforeEach(async () => {
    ctx = await fixture({ codexBin: path.resolve('tests/fixtures/codex.mjs') })
    ctx.service.task({ ...ctx.task, prompt: 'fixture:restart' }, ctx.task.id)
  })
  afterEach(async () => {
    for (const child of processes.splice(0)) {
      if (child.exitCode !== null || child.signalCode !== null)
        continue
      const stopped = new Promise(resolve => child.once('close', resolve))
      child.kill('SIGTERM')
      await stopped
    }

    for (const worker of workers.splice(0))
      await worker.close()
    await ctx.dispose()
  })
  const waitForWork = async (id: string) => {
    await expect.poll(async () => readFile(path.join(ctx.service.store.run(id)?.workspace ?? ctx.projectPath, 'restart-work.txt'), 'utf8').catch(() => ''), { timeout: 10000 }).toBe('preserved before restart')
    await expect.poll(() => ctx.service.store.run(id)?.sessionId, { timeout: 10000 }).toBe('fixture-session')
  }

  const restart = async () => {
    const worker = new Worker(ctx.service)
    workers.push(worker)
    await worker.tick()
    return worker
  }

  const succeeded = async (id: string) => {
    await expect.poll(async () => {
      // These workers have no interval. Recovery can defer a run while its old
      // process is being fenced, so keep scheduling as the real worker does.
      for (const worker of workers)
        await worker.tick()
      return ctx.service.store.run(id)?.status
    }, { timeout: 10000 }).toBe('succeeded')
    expect(ctx.service.store.run(id)?.summary).toContain('saved conversation and work survived')
  }

  it('pauses on shutdown and resumes the same run, session, workspace and budget', async () => {
    ctx.service.agent({ ...ctx.agent, timeoutMinutes: 120 }, ctx.agent.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    const paused = ctx.service.store.run(run.id)!
    const budget = ctx.worker.recovery.get(run.id)!.remainingMs!
    expect(paused).toMatchObject({
      status: 'queued',
      recoveryPending: true,
      sessionId: 'fixture-session',
      finishedAt: null,
    })
    expect(budget).toBeLessThan(run.snapshot.agent.timeoutMinutes * 60000)
    const worker = await restart()
    await succeeded(run.id)
    expect(ctx.service.store.run(run.id)).toMatchObject({ startedAt: paused.startedAt, workspace: paused.workspace, resumeCount: 1 })
    expect(worker.recovery.get(run.id)!.remainingMs).toBeLessThan(budget)
    expect(ctx.service.store.runs()).toHaveLength(1)
  })

  it.each(['SIGTERM', 'SIGKILL'] as const)('recovers after a real manager process receives %s', async (signal) => {
    const run = await ctx.service.enqueue(ctx.task.id)
    const child = spawn(process.execPath, ['--import', import.meta.resolve('tsx'), path.resolve('tests/fixtures/worker-process.ts')], { stdio: ['ignore', 'ignore', 'pipe', 'ipc'] })
    processes.push(child)
    let diagnostics = ''
    child.stderr?.on('data', chunk => diagnostics += chunk)
    child.send(ctx.service.config)
    await waitForWork(run.id)
    // The child persists the session ID before its event; SIGKILL can land between
    // those writes. Wait for the event that this test counts after recovery.
    await expect.poll(() => ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(1)
    const owner = ctx.worker.recovery.get(run.id)!.process!
    const stopped = new Promise(resolve => child.once('close', resolve))
    child.kill(signal)
    await stopped
    expect(diagnostics).not.toContain('Error')
    await restart()
    await succeeded(run.id)
    await expect.poll(() => processIdentity(owner.pid)).toBeUndefined()
    expect(ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(2)
  })

  it('keeps explicit cancellations stopped and supports an explicit resume on the same run', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    ctx.worker.cancel(run.id)
    await ctx.worker.close()
    const worker = await restart()
    expect(ctx.service.store.run(run.id)?.status).toBe('cancelled')
    const headers = await ctx.login()
    const response = await ctx.app.inject({ method: 'POST', url: `/api/runs/${run.id}/resume`, headers })
    expect(response.statusCode, response.body).toBe(200)
    await worker.tick()
    await succeeded(run.id)
  })

  it('honors cancellation persisted immediately before a crash', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    ctx.service.store.updateRun(run.id, { status: 'running', cancelRequestedAt: Date.now() })
    await restart()
    expect(ctx.service.store.run(run.id)?.status).toBe('cancelled')
    expect(ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(1)
  })

  it('honors a queued cancellation even if shutdown interrupted the status update', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    ctx.service.store.updateRun(run.id, { cancelRequestedAt: Date.now() })
    await restart()
    expect(ctx.service.store.run(run.id)?.status).toBe('cancelled')
    expect(ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(0)
  })

  it('resumes isolated execution only after the old container stops and retains its private home', async () => {
    const provider = await runnerProvider(ctx.service.config.dataDir)
    const data = await accountFixture(ctx)
    data.seed('Private account')
    ctx.service.config.runnerUrl = provider.url
    const agent = ctx.service.agent({ name: 'Isolated recovery', access: { projects: [ctx.project.id], skills: [], github: false } })
    const task = ctx.service.task({ ...ctx.task, agentId: agent.id, prompt: 'fixture:restart' }, ctx.task.id)
    const run = await ctx.service.enqueue(task.id)
    try {
      await ctx.worker.tick()
      await waitForWork(run.id)
      provider.control.available = false
      await ctx.worker.close()
      const home = path.join(ctx.service.config.dataDir, 'runs', run.id, 'home', '.codex')
      expect(await readFile(path.join(home, 'auth.json'), 'utf8')).toContain('synthetic')
      const worker = await restart()
      expect(ctx.service.store.run(run.id)).toMatchObject({ status: 'queued', recoveryPending: true })
      expect(provider.attempts.size).toBe(1)
      provider.control.available = true
      await worker.tick()
      await succeeded(run.id)
      await expect.poll(() => worker.active.size).toBe(0)
      expect(provider.attempts.size).toBe(2)
      expect([...provider.attempts.values()].every(attempt => attempt.closed)).toBe(true)
      expect(await readFile(path.join(home, 'fixture-conversation.json'), 'utf8')).toContain('fixture-session')
      await expect(readFile(path.join(home, 'auth.json'))).rejects.toThrow()
    }
    finally {
      await provider.close()
    }
  })

  it('discovers a saved thread if its first notification was lost', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    ctx.service.store.updateRun(run.id, { sessionId: null })
    await restart()
    await succeeded(run.id)
  })

  it('preserves files and fails visibly when conversation history is missing', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    await rm(path.join(ctx.service.config.dataDir, 'runs', run.id, 'codex', 'fixture-conversation.json'))
    await restart()
    await expect.poll(() => ctx.service.store.run(run.id)?.status).toBe('failed')
    expect(ctx.service.store.run(run.id)?.summary).toContain('saved Codex conversation is unavailable')
    expect(ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(1)
    expect(await readFile(path.join(ctx.projectPath, 'restart-work.txt'), 'utf8')).toBe('preserved before restart')
  })

  it('finalizes a checkpointed completion without replaying the conversation', async () => {
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    const checkpoint = ctx.worker.recovery.get(run.id)!
    checkpoint.completed = true
    checkpoint.lastMessage = 'Already completed before the crash'
    ctx.worker.recovery.save(run.id, checkpoint)
    await restart()
    expect(ctx.service.store.run(run.id)).toMatchObject({ status: 'succeeded', summary: checkpoint.lastMessage })
    expect(ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(1)
  })

  it('selects fresh available capacity when resuming a stopped conversation', async () => {
    const data = await accountFixture(ctx)
    const original = data.seed('Original', limits(10, 10))
    const replacement = data.seed('Replacement', limits(30, 30))
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    expect(ctx.service.store.run(run.id)?.codexAccountId).toBe(original.id)
    await ctx.worker.close()
    data.pool.markExhausted(original.id, '')
    await restart()
    await succeeded(run.id)
    expect(ctx.service.store.run(run.id)?.codexAccountId).toBe(replacement.id)
  })

  it('retains credentials and project/account locks until the isolated runner confirms shutdown', async () => {
    let available = false
    const requests: string[] = []
    const broker = http.createServer((request, response) => {
      requests.push(`${request.method} ${request.url}`)
      response.writeHead(available ? 200 : 503).end('{}')
    })
    await new Promise<void>(resolve => broker.listen(0, '127.0.0.1', resolve))
    try {
      const data = await accountFixture(ctx)
      const account = data.seed('Original')
      const run = await ctx.service.enqueue(ctx.task.id)
      await ctx.worker.tick()
      await waitForWork(run.id)
      await ctx.worker.close()
      ctx.service.config.runnerUrl = `http://127.0.0.1:${(broker.address() as { port: number }).port}`
      ctx.service.store.updateRun(run.id, { isolated: true })
      const checkpoint = ctx.worker.recovery.get(run.id)!
      checkpoint.runnerId = '00000000-0000-4000-8000-000000000001'
      checkpoint.completed = true
      ctx.worker.recovery.save(run.id, checkpoint)
      const auth = path.join(ctx.service.config.dataDir, 'runs', run.id, 'codex', 'auth.json')
      await writeFile(auth, JSON.stringify(credential(account.id)))
      const otherTask = ctx.service.task({ ...ctx.task, name: 'Waiting for project', prompt: 'Complete fixture' })
      const other = await ctx.service.enqueue(otherTask.id)
      const recover = vi.spyOn(data.pool, 'recoverRun')
      const worker = await restart()
      expect(ctx.service.store.run(run.id)).toMatchObject({ status: 'queued', recoveryPending: true })
      expect(ctx.service.store.run(other.id)?.status).toBe('queued')
      expect(recover).not.toHaveBeenCalled()
      expect(await readFile(auth, 'utf8')).toContain('synthetic')
      await expect(data.pool.acquire('unrelated-run')).rejects.toThrow(/Waiting/)
      await expect(data.pool.remove(account.id)).rejects.toThrow(/finish/)
      available = true
      await worker.tick()
      expect(ctx.service.store.run(run.id)?.status).toBe('succeeded')
      expect(recover).toHaveBeenCalledOnce()
      await expect(readFile(auth)).rejects.toThrow()
      expect(requests).toEqual([`DELETE /runs/${checkpoint.runnerId}`, `DELETE /runs/${checkpoint.runnerId}`])
    }
    finally {
      await new Promise<void>((resolve, reject) => broker.close(error => error ? reject(error) : resolve()))
    }
  })

  it('does not grant a fresh timeout or broaden changed agent permissions on automatic resume', async () => {
    ctx.service.agent({ ...ctx.agent, timeoutMinutes: 120 }, ctx.agent.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    const checkpoint = ctx.worker.recovery.get(run.id)!
    checkpoint.remainingMs = 0
    ctx.worker.recovery.save(run.id, checkpoint)
    const worker = await restart()
    await expect.poll(() => ctx.service.store.run(run.id)?.status).toBe('failed')
    expect(ctx.service.store.run(run.id)?.summary).toContain('time limit')
    await expect.poll(() => worker.active.size).toBe(0)
    worker.resume(run.id)
    expect(worker.recovery.get(run.id)!.remainingMs).toBe(run.snapshot.agent.timeoutMinutes * 60000)
    ctx.service.agent({ ...ctx.agent, access: { projects: [], skills: [], github: false } }, ctx.agent.id)
    await worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status).toBe('failed')
    expect(ctx.service.store.run(run.id)?.summary).toContain('access changed')
    expect(ctx.service.store.events(run.id).filter(event => event.type === 'thread.started')).toHaveLength(1)
  })

  const initializeGit = async () => {
    const exec = promisify(execFile)
    await exec('git', ['init', '-b', 'main', ctx.projectPath])
    await writeFile(path.join(ctx.projectPath, 'README.md'), 'original')
    await exec('git', ['-C', ctx.projectPath, 'add', '.'])
    await exec('git', ['-C', ctx.projectPath, '-c', 'user.name=Test', '-c', 'user.email=test@example.com', 'commit', '-m', 'Initial'])
    await writeFile(path.join(ctx.projectPath, 'README.md'), 'unrelated dirty source')
    ctx.service.task({ ...ctx.task, prompt: 'fixture:restart', worktree: true }, ctx.task.id)
  }

  it('resumes a dirty managed worktree without recreating it or touching the source checkout', async () => {
    await initializeGit()
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await waitForWork(run.id)
    await ctx.worker.close()
    const workspace = ctx.service.store.run(run.id)!.workspace!
    await restart()
    await succeeded(run.id)
    expect(ctx.service.store.run(run.id)?.workspace).toBe(workspace)
    expect(await readFile(path.join(workspace, 'restart-work.txt'), 'utf8')).toBe('preserved before restart')
    expect(await readFile(path.join(ctx.projectPath, 'README.md'), 'utf8')).toBe('unrelated dirty source')
  })

  it('preserves an incomplete workspace preparation and uses a new generation', async () => {
    await initializeGit()
    ctx.service.task({ ...ctx.task, prompt: 'Complete fixture task', worktree: true }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    const abandoned = await prepareExecution(run, ctx.service.config)
    await writeFile(path.join(abandoned.cwd, 'partial-work.txt'), 'preserve incomplete attempt')
    ctx.worker.recovery.save(run.id, { launched: false, remainingMs: 60000 })
    ctx.service.store.updateRun(run.id, { status: 'running', startedAt: Date.now() })
    const worker = await restart()
    await expect.poll(() => ctx.service.store.run(run.id)?.status).toBe('succeeded')
    expect(ctx.service.store.run(run.id)?.workspace).not.toBe(abandoned.cwd)
    expect(await readFile(path.join(abandoned.cwd, 'partial-work.txt'), 'utf8')).toBe('preserve incomplete attempt')
    await expect.poll(() => worker.active.size).toBe(0)
    await worker.cleanup(run.id)
    expect(ctx.service.store.run(run.id)?.workspaceCleanedAt).toBeTypeOf('number')
  })
})
