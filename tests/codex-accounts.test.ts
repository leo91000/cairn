import {
  mkdir,
  readFile,
  stat,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { accountFixture, credential, limits } from './codex-account-fixture'
import { fixture } from './helpers'
import { remainingUsage, usageRecovered } from './legacy/server/codex-account-types'
import { CodexAccounts } from './legacy/server/codex-accounts'
import { codexSession } from './legacy/server/codex-rpc'
import { prepareExecution } from './legacy/server/execution'
import { codexArgs, usageExhausted } from './legacy/server/worker'

const binary = path.resolve('tests/fixtures/codex.mjs')
describe('codex account pool', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  let data: Awaited<ReturnType<typeof accountFixture>>
  beforeEach(async () => {
    ctx = await fixture({ codexBin: binary })
    data = await accountFixture(ctx)
  })
  afterEach(async () => {
    await ctx.dispose()
  })

  it('ranks by the tightest window, reserves accounts, and never falls back after pool activation', async () => {
    const short = data.seed('Short window', limits(5, 95))
    const balanced = data.seed('Balanced', limits(30, 40))
    const lease = (await data.pool.acquire(ctx.task.id))!
    expect(lease.accountId).toBe(balanced.id)
    const second = (await data.pool.acquire(ctx.agent.id))!
    expect(second.accountId).toBe(short.id)
    await expect(data.pool.acquire(ctx.project.id)).rejects.toThrow(/Waiting/)
    await expect(data.pool.remove(balanced.id)).rejects.toThrow(/finish/)
    await expect(data.pool.startLogin('Reconnect', balanced.id)).rejects.toThrow(/finish/)
    expect(JSON.parse(await readFile(path.join(lease.home, 'auth.json'), 'utf8')).tokens.account_id).toBe(balanced.id)
    expect((await stat(path.join(lease.home, 'auth.json'))).mode & 0o777).toBe(0o600)
    await data.pool.release(lease)
    await data.pool.release(second)
    await expect(readFile(path.join(lease.home, 'auth.json'))).rejects.toThrow()
    for (const account of data.pool.list())
      await data.pool.remove(account.id)
    await expect(data.pool.acquire(ctx.task.id)).rejects.toThrow(/Waiting/)
  })

  it('does not select disabled, exhausted, unknown or server-blocked capacity', async () => {
    const disabled = data.seed('Paused', limits(0, 0))
    data.pool.update(disabled.id, { name: disabled.name, enabled: false })
    data.seed('Blocked', { ...limits(), ordinaryUsageAllowed: false })
    data.seed('Unknown', { rateLimits: { ...limits().rateLimits, primary: null, secondary: null } })
    data.seed('Empty', limits(100, 0))
    const healthy = data.seed('Available', limits(80, 80))
    const lease = (await data.pool.acquire(ctx.task.id))!
    expect(lease.accountId).toBe(healthy.id)
    await data.pool.release(lease)
    data.pool.markExhausted(healthy.id, '')
    await expect(data.pool.acquire(ctx.task.id)).rejects.toThrow(/Waiting/)
  })

  it('respects model-specific buckets', async () => {
    const value = { ...limits(0, 0), rateLimitsByLimitId: { expensive: { ...limits(99, 0).rateLimits, normalModelSlug: 'special-model' } } }
    data.seed('Model-limited', value)
    const balanced = data.seed('Balanced', limits(40, 40))
    expect(remainingUsage(value, 'special-model')).toBe(1)
    const lease = (await data.pool.acquire(ctx.task.id, 'special-model'))!
    expect(lease.accountId).toBe(balanced.id)
    await data.pool.release(lease)
  })

  it('requires fresh usage to confirm natural resets when no banked resets are available', async () => {
    const account = data.seed('Empty', limits(100, 20))
    data.pool.markExhausted(account.id, '')
    const expired = limits(100, 20)
    expired.rateLimits.primary!.resetsAt = Math.floor(Date.now() / 1000) - 1
    data.responses.set(account.id, expired)
    await data.pool.poll()
    expect(data.pool.get(account.id).exhausted).not.toBeNull()
    await expect(data.pool.acquire(ctx.task.id)).rejects.toThrow(/Waiting/)
    data.responses.set(account.id, limits(0, 20))
    await data.pool.poll()
    expect(data.pool.get(account.id).exhausted).toBeNull()
    const lease = (await data.pool.acquire(ctx.task.id))!
    await data.pool.release(lease)
    expect(new Set(data.calls)).toEqual(new Set(['account/read', 'account/rateLimits/read']))
  })

  it('persists rotated credentials even on failed reads and hides provider errors and secrets', async () => {
    const account = data.seed('Personal')
    data.responses.set(account.id, new Error('synthetic-sensitive-provider-response'))
    await data.pool.refresh(account.id)
    expect(data.pool.get(account.id).state).toBe('error')
    expect(JSON.stringify(data.pool.list())).not.toContain('synthetic-sensitive')
    expect(JSON.stringify(data.pool.vault.get(`codex-account:${account.id}`))).toContain('synthetic-rotated')
    expect(JSON.stringify(data.pool.list())).not.toMatch(/access_token|refresh_token|identity/)
    await expect(readFile(path.join(ctx.service.config.dataDir, 'codex-monitor', account.id, 'auth.json'))).rejects.toThrow()
    await expect(data.pool.acquire(ctx.task.id)).rejects.toThrow(/Waiting/)
  })

  it('reads the leased credentials and recovers refreshed credentials after a restart', async () => {
    const account = data.seed('Personal')
    const lease = (await data.pool.acquire(ctx.task.id))!
    await data.pool.refresh(account.id)
    expect(JSON.parse(await readFile(path.join(lease.home, 'auth.json'), 'utf8')).tokens.refresh_token).toContain('rotated')
    await data.pool.release(lease)
    const run = await ctx.service.enqueue(ctx.task.id)
    ctx.service.store.updateRun(run.id, { codexAccountId: account.id, status: 'interrupted' })
    const home = path.join(ctx.service.config.dataDir, 'runs', run.id, 'codex')
    await mkdir(home, { recursive: true })
    const rotated = credential(account.id)
    rotated.tokens.refresh_token = 'synthetic-recovered'
    await writeFile(path.join(home, 'auth.json'), JSON.stringify(rotated))
    const restarted = new CodexAccounts(ctx.service.store, ctx.service.config, data.pool.session)
    await restarted.initialize()
    expect(JSON.stringify(restarted.vault.get(`codex-account:${account.id}`))).toContain('synthetic-recovered')
    await expect(readFile(path.join(home, 'auth.json'))).rejects.toThrow()
    await restarted.close()
  })

  it('recovers a rotated credential left by an interrupted usage monitor', async () => {
    const account = data.seed('Personal')
    const home = path.join(ctx.service.config.dataDir, 'codex-monitor', account.id)
    await mkdir(home, { recursive: true })
    const auth = credential(account.id)
    auth.tokens.refresh_token = 'synthetic-monitor-recovered'
    await writeFile(path.join(home, 'auth.json'), JSON.stringify(auth))
    const restarted = new CodexAccounts(ctx.service.store, ctx.service.config, data.pool.session)
    await restarted.initialize()
    expect(JSON.stringify(restarted.vault.get(`codex-account:${account.id}`))).toContain('synthetic-monitor-recovered')
    await expect(readFile(path.join(home, 'auth.json'))).rejects.toThrow()
    await restarted.close()
  })

  it('rejects credentials for another identity without overwriting the saved account', async () => {
    const account = data.seed('Personal')
    const lease = (await data.pool.acquire(ctx.task.id))!
    await writeFile(path.join(lease.home, 'auth.json'), JSON.stringify(credential('another-account')))
    await expect(data.pool.release(lease)).rejects.toThrow(/different account/)
    expect(JSON.stringify(data.pool.vault.get(`codex-account:${account.id}`))).toContain(account.id)
    expect(data.pool.leases.size).toBe(0)
  })

  it('offers authenticated account CRUD and verifies device sign-in using the actual stdio adapter', async () => {
    ctx.service.accounts = new CodexAccounts(ctx.service.store, ctx.service.config)
    const headers = await ctx.login()
    expect((await ctx.app.inject('/api/codex/accounts')).statusCode).toBe(401)
    expect((await ctx.app.inject({
      method: 'POST',
      url: '/api/codex/accounts/login',
      headers: { cookie: headers.cookie },
      payload: { name: 'Personal' },
    })).statusCode).toBe(403)
    const login = await ctx.app.inject({
      method: 'POST',
      url: '/api/codex/accounts/login',
      headers,
      payload: { name: 'Personal' },
    })
    expect(login.statusCode, login.body).toBe(200)
    const id = login.json().accountId
    await expect.poll(() => ctx.service.accounts.flow()?.state).toBe('complete')
    const response = await ctx.app.inject({ url: '/api/codex/accounts', headers })
    expect(response.json()).toMatchObject([{ name: 'Personal', remainingPercent: 60, state: 'ready' }])
    expect(response.body).not.toMatch(/synthetic|access_token|identity/)
    // The legacy adapter reports verified sign-in before its asynchronous
    // credential-directory cleanup releases the account for editing.
    await expect.poll(async () => {
      const updated = await ctx.app.inject({
        method: 'PUT',
        url: `/api/codex/accounts/${id}`,
        headers,
        payload: { name: 'Renamed', enabled: false },
      })
      return { status: updated.statusCode, ...updated.json() }
    }).toMatchObject({ status: 200, name: 'Renamed', enabled: false })
    expect((await ctx.app.inject({ method: 'DELETE', url: `/api/codex/accounts/${id}`, headers })).statusCode).toBe(200)
    expect(ctx.service.accounts.list()).toEqual([])
  })

  it('polls every minute and recognizes resets even when a second window still limits capacity', async () => {
    data.seed('Personal', limits(97, 98))
    vi.useFakeTimers()
    try {
      data.pool.start()
      await data.pool.poll()
      const before = data.calls.length
      await vi.advanceTimersByTimeAsync(60000)
      await data.pool.poll()
      expect(data.calls.length).toBeGreaterThan(before)
    }
    finally {
      await data.pool.close()
      vi.useRealTimers()
    }

    expect(usageRecovered(limits(97, 98), limits(0, 98), '')).toBe(true)
    expect(usageRecovered(limits(97, 98), limits(97, 98), '')).toBe(false)
  })

  it('passes only the selected credential to isolated execution and preserves its session when switching', async () => {
    const first = data.seed('First', limits(0, 0))
    const second = data.seed('Second', limits(20, 20))
    const agent = ctx.service.agent({ name: 'Restricted', access: { projects: [ctx.project.id], github: false, sandbox: 'workspace-write' } })
    const task = ctx.service.task({ ...ctx.task, agentId: agent.id })
    const run = await ctx.service.enqueue(task.id)
    const lease = (await data.pool.acquire(run.id))!
    const prepared = await prepareExecution(run, { ...ctx.service.config, runnerUrl: 'http://runner' }, undefined, lease.home)
    const mount = prepared.mounts.find(mount => mount.target === '/home/node')!
    const home = path.join(mount.source, '.codex')
    await data.pool.relocate(lease, home)
    expect(JSON.parse(await readFile(path.join(home, 'auth.json'), 'utf8')).tokens.account_id).toBe(first.id)
    expect(prepared.mounts.some(mount => mount.source === ctx.service.config.dataDir || mount.source === ctx.home)).toBe(false)
    await writeFile(path.join(home, 'session-marker'), 'conversation')
    data.pool.markExhausted(first.id, '')
    await data.pool.release(lease)
    const next = (await data.pool.acquire(run.id))!
    expect(next.accountId).toBe(second.id)
    await data.pool.relocate(next, home)
    expect(await readFile(path.join(home, 'session-marker'), 'utf8')).toBe('conversation')
    expect(JSON.parse(await readFile(path.join(home, 'auth.json'), 'utf8')).tokens.account_id).toBe(second.id)
    expect(codexArgs(run, prepared.output, 'saved-session')).toEqual(expect.arrayContaining(['--sandbox', 'workspace-write', '-a', 'never']))
    await data.pool.release(next)
  })

  it('imports the existing ChatGPT login once and stores the pool credential encrypted', async () => {
    await mkdir(path.join(ctx.home, '.codex'), { recursive: true })
    await writeFile(path.join(ctx.home, '.codex/auth.json'), JSON.stringify(credential('legacy')))
    const imported = new CodexAccounts(ctx.service.store, ctx.service.config, data.pool.session)
    await imported.initialize()
    await imported.poll()
    expect(imported.list()).toMatchObject([{ name: 'Primary account', state: 'ready', remainingPercent: 60 }])
    const id = imported.list()[0].id
    expect(ctx.service.store.kv(`mcp-secret:codex-account:${id}`)).not.toContain('synthetic')
    const restarted = new CodexAccounts(ctx.service.store, ctx.service.config, data.pool.session)
    await restarted.initialize()
    expect(restarted.list()).toHaveLength(1)
    await imported.close()
    await restarted.close()
  })

  it('keeps the original timeout while waiting for a natural reset', async () => {
    data.seed('Empty soon')
    ctx.service.task({ ...ctx.task, prompt: 'fixture:exhaust' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    run.snapshot.agent.timeoutMinutes = 0.015
    ctx.service.store.updateRun(run.id, { snapshot: run.snapshot })
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 4000 }).toBe('failed')
    expect(ctx.service.store.run(run.id)?.summary).toContain('time limit')
    expect(data.pool.leases.size).toBe(0)
  })

  it('queues new work with an explanation when the managed pool has no usable account', async () => {
    data.seed('Exhausted', limits(100, 0))
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    expect(ctx.service.store.run(run.id)).toMatchObject({ status: 'queued', accountWaitReason: expect.stringContaining('Waiting') })
    expect(ctx.worker.active.size).toBe(0)
  })

  it('propagates a missing CLI as a sanitized error and closes the session', async () => {
    await expect(codexSession({ ...ctx.service.config, codexBin: '/missing/codex' })(ctx.home, rpc => rpc.request('account/read'))).rejects.toThrow(/Codex account service/)
  })

  it('resumes the same run, conversation and workspace with the next account after exhaustion', async () => {
    const first = data.seed('First', limits(0, 10))
    const second = data.seed('Second', limits(20, 30))
    ctx.service.task({ ...ctx.task, prompt: 'fixture:exhaust' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 6000 }).toBe('succeeded')
    await Promise.all(ctx.worker.executions)
    const result = ctx.service.store.run(run.id)!
    expect(result.codexAccountId).toBe(second.id)
    expect(result.sessionId).toBe('fixture-session')
    expect(data.pool.get(first.id).exhausted).not.toBeNull()
    expect(ctx.service.store.events(run.id).some(event => event.text.includes('workspace intact'))).toBe(true)
    expect(data.pool.leases.size).toBe(0)
    expect(codexArgs(result, '/output', result.sessionId!)).toEqual(expect.arrayContaining(['exec', 'resume', 'fixture-session', '--json']))
    expect(codexArgs(result, '/output', result.sessionId!)).not.toContain('--color')
  })

  it('waits on exhaustion when all accounts are unavailable and remains cancellable', async () => {
    data.seed('Only account')
    ctx.service.task({ ...ctx.task, prompt: 'fixture:exhaust' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.accountWaitReason).toMatch(/Waiting/)
    ctx.worker.cancel(run.id)
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 3000 }).toBe('cancelled')
    expect(data.pool.leases.size).toBe(0)
  })

  it('does not confuse command output, model failures or temporary throttling with exhaustion', () => {
    for (const event of [{ type: 'item.completed', item: { text: 'You\'ve hit your usage limit.' } }, { type: 'turn.failed', error: { message: '429 Too Many Requests' } }, { type: 'turn.failed', error: { message: 'Connection failed' } }])
      expect(usageExhausted(event)).toBe(false)
    expect(usageExhausted({ type: 'turn.failed', error: { message: 'You\'ve hit your usage limit. Try again later.' } })).toBe(true)
  })
})
