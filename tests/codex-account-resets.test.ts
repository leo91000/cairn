import path from 'node:path'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { accountFixture, limits } from './codex-account-fixture'
import { fixture } from './helpers'
import { CodexAccounts } from './legacy/server/codex-accounts'

const banked = (primary = 98, secondary = 40, availableCount = 3) => ({ ...limits(primary, secondary), rateLimitResetCredits: { availableCount } })

describe('automatic banked resets', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  let data: Awaited<ReturnType<typeof accountFixture>>
  beforeEach(async () => {
    ctx = await fixture({ codexBin: path.resolve('tests/fixtures/codex.mjs') })
    data = await accountFixture(ctx)
  })
  afterEach(async () => {
    await ctx.dispose()
  })

  it('redeems at 2% while idle and selects the earliest-expiring usable credit', async () => {
    const account = data.seed('Idle', {
      ...banked(),
      rateLimitResetCredits: {
        availableCount: 3,
        credits: [
          {
            id: 'later',
            status: 'available',
            resetType: 'codexRateLimits',
            expiresAt: Date.now() / 1000 + 86400,
          },
          {
            id: 'expired',
            status: 'available',
            resetType: 'codexRateLimits',
            expiresAt: 1,
          },
          {
            id: 'used',
            status: 'redeemed',
            resetType: 'codexRateLimits',
            expiresAt: Date.now() / 1000 + 10,
          },
          {
            id: 'soon',
            status: 'available',
            resetType: 'codexRateLimits',
            expiresAt: Date.now() / 1000 + 3600,
          },
        ],
      },
    })
    data.consume.set(account.id, () => {
      data.responses.set(account.id, banked(0, 0, 2))
      return { outcome: 'reset' }
    })
    await Promise.all([data.pool.poll(), data.pool.refresh(account.id), data.pool.refresh(account.id)])
    expect(data.redemptions).toHaveLength(1)
    expect(data.redemptions[0].params).toMatchObject({ creditId: 'soon', idempotencyKey: expect.any(String) })
    expect(data.pool.list()[0]).toMatchObject({ remainingPercent: 100, limits: { rateLimitResetCredits: { availableCount: 2 } }, resetError: '' })
    expect(data.pool.leases.size).toBe(0)
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(1)
  })

  it.each([
    ['above threshold', banked(97.9), true],
    ['paused', banked(100), false],
    ['no credits', banked(100, 40, 0), true],
    ['older CLI', limits(100), true],
    ['unknown usage', { ...banked(), rateLimits: { ...limits().rateLimits, primary: null, secondary: null } }, true],
  ])('does not spend resets for %s', async (_, value, enabled) => {
    const account = data.seed('Account', value)
    data.pool.update(account.id, { name: account.name, enabled })
    await data.pool.poll()
    expect(data.redemptions).toEqual([])
    expect(data.pool.get(account.id).state).toBe('ready')
  })

  it('checks the weekly window and the active model without interrupting its lease', async () => {
    const account = data.seed('Active', banked(0, 0))
    const lease = (await data.pool.acquire(ctx.task.id, 'special-model'))!
    data.responses.set(account.id, { ...banked(0, 0), rateLimitsByLimitId: { special: { ...limits(0, 99).rateLimits, normalModelSlug: 'special-model' } } })
    data.consume.set(account.id, () => {
      data.responses.set(account.id, banked(0, 0, 2))
      return { outcome: 'reset' }
    })
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(1)
    expect(data.pool.leases.get(account.id)).toBe(lease)
    await data.pool.release(lease)
  })

  it('never redeems usage belonging to a different account', async () => {
    const account = data.seed('Account', banked())
    data.responses.set(account.id, { ...banked(), accountId: 'different-account' })
    await data.pool.poll()
    expect(data.redemptions).toEqual([])
    expect(data.pool.get(account.id).state).toBe('error')
  })

  it('uses fresh natural recovery before considering a banked reset for an exhausted account', async () => {
    const account = data.seed('Account', banked(100))
    data.pool.markExhausted(account.id, '')
    data.responses.set(account.id, banked(0))
    await data.pool.poll()
    expect(data.redemptions).toEqual([])
    expect(data.pool.get(account.id).exhausted).toBeNull()
  })

  it('reuses the persisted request after a lost response and worker restart', async () => {
    const account = data.seed('Account', banked())
    data.consume.set(account.id, () => {
      throw new Error('sensitive backend timeout')
    })
    await data.pool.refresh(account.id)
    const original = data.redemptions[0].params
    expect(data.pool.get(account.id)).toMatchObject({ state: 'ready', resetError: expect.stringContaining('Retrying automatically') })
    expect(JSON.stringify(data.pool.list())).not.toContain('sensitive')
    const lease = (await data.pool.acquire(ctx.task.id))!
    await data.pool.release(lease)
    await data.pool.close()
    const restarted = new CodexAccounts(ctx.service.store, ctx.service.config, data.pool.session)
    ctx.service.accounts = restarted
    data.responses.set(account.id, banked(98, 40, 0))
    data.consume.set(account.id, () => {
      data.responses.set(account.id, banked(0, 0, 0))
      return { outcome: 'alreadyRedeemed' }
    })
    await restarted.poll()
    expect(data.redemptions).toHaveLength(2)
    expect(data.redemptions[1].params).toEqual(original)
    expect(restarted.get(account.id).resetError).toBe('')
    expect(ctx.service.store.kv(`codex-reset:${account.id}`)).toBeUndefined()
  })

  it('does not redeem repeatedly on delayed low readings or a failed post-reset read', async () => {
    const account = data.seed('Account', banked())
    data.consume.set(account.id, () => {
      data.responses.set(account.id, new Error('delayed usage'))
      return { outcome: 'reset' }
    })
    await data.pool.refresh(account.id)
    expect(data.pool.get(account.id).state).toBe('ready')
    data.responses.set(account.id, banked(98, 40, 2))
    await data.pool.poll()
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(1)
    expect(data.pool.get(account.id).resetError).toContain('waiting for refreshed capacity')
    data.responses.set(account.id, banked(0, 0, 2))
    await data.pool.poll()
    data.responses.set(account.id, banked(0, 99, 2))
    data.consume.set(account.id, () => {
      data.responses.set(account.id, banked(0, 0, 1))
      return { outcome: 'reset' }
    })
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(2)
    expect(data.redemptions[0].params.idempotencyKey).not.toBe(data.redemptions[1].params.idempotencyKey)
  })

  it('does not spend on a healthy account after an ambiguous failed request', async () => {
    const account = data.seed('Account', banked())
    data.consume.set(account.id, () => {
      throw new Error('timeout')
    })
    await data.pool.poll()
    data.responses.set(account.id, banked(0, 0, 2))
    await data.pool.poll()
    data.pool.update(account.id, { name: account.name, enabled: false })
    data.responses.set(account.id, banked(100, 0, 2))
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(1)
    data.pool.update(account.id, { name: account.name, enabled: true })
    data.consume.set(account.id, () => {
      data.responses.set(account.id, banked(0, 0, 1))
      return { outcome: 'reset' }
    })
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(2)
    expect(data.redemptions[1].params.idempotencyKey).not.toBe(data.redemptions[0].params.idempotencyKey)
  })

  it.each(['nothingToReset', 'noCredit'])('keeps capacity usable after %s and can retry a new attempt later', async (outcome) => {
    const account = data.seed('Account', banked())
    data.consume.set(account.id, () => ({ outcome }))
    await data.pool.poll()
    expect(data.pool.get(account.id).state).toBe('ready')
    expect(ctx.service.store.kv(`codex-reset:${account.id}`)).toBeUndefined()
    const lease = (await data.pool.acquire(ctx.task.id))!
    await data.pool.release(lease)
    await data.pool.poll()
    expect(data.redemptions).toHaveLength(2)
    expect(data.redemptions[0].params.idempotencyKey).not.toBe(data.redemptions[1].params.idempotencyKey)
  })

  it('polls low accounts every 15 seconds while healthy accounts stay on one-minute checks', async () => {
    const low = data.seed('Low', banked(90))
    const healthy = data.seed('Healthy', banked(20))
    const refresh = vi.spyOn(data.pool, 'refresh')
    vi.useFakeTimers()
    try {
      data.pool.start()
      await data.pool.poll()
      refresh.mockClear()
      await vi.advanceTimersByTimeAsync(15000)
      await data.pool.poll(true)
      expect(refresh.mock.calls.map(([id]) => id)).toEqual([low.id])
      for (let tick = 0; tick < 3; tick++) {
        await vi.advanceTimersByTimeAsync(15000)
        await data.pool.poll(true)
      }

      expect(refresh.mock.calls.filter(([id]) => id === healthy.id)).toHaveLength(1)
    }
    finally {
      await data.pool.close()
      vi.useRealTimers()
    }
  })

  it('resumes the same saved conversation on the reset account when a burst exhausts usage between polls', async () => {
    const account = data.seed('Only account', banked(0, 0))
    data.consume.set(account.id, () => {
      data.responses.set(account.id, banked(0, 0, 2))
      return { outcome: 'reset' }
    })
    ctx.service.task({ ...ctx.task, prompt: 'fixture:exhaust fixture:banked-reset' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 6000 }).toBe('succeeded')
    await Promise.all(ctx.worker.executions)
    expect(ctx.service.store.run(run.id)).toMatchObject({ codexAccountId: account.id, sessionId: 'fixture-session' })
    expect(data.redemptions).toHaveLength(1)
    expect(data.pool.get(account.id).exhausted).toBeNull()
    expect(data.pool.leases.size).toBe(0)
  })

  it('switches accounts when banked reset redemption fails after exhaustion', async () => {
    const account = data.seed('First', banked(0, 0))
    const backup = data.seed('Backup', limits(20, 20))
    data.consume.set(account.id, () => {
      throw new Error('reset service unavailable')
    })
    ctx.service.task({ ...ctx.task, prompt: 'fixture:exhaust' }, ctx.task.id)
    const run = await ctx.service.enqueue(ctx.task.id)
    await ctx.worker.tick()
    await expect.poll(() => ctx.service.store.run(run.id)?.status, { timeout: 6000 }).toBe('succeeded')
    await Promise.all(ctx.worker.executions)
    expect(ctx.service.store.run(run.id)?.codexAccountId).toBe(backup.id)
    expect(data.redemptions).toHaveLength(1)
    expect(data.pool.get(account.id).exhausted).not.toBeNull()
  })
})
