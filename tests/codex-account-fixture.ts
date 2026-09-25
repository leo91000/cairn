import type { fixture } from './helpers'
import type { AccountLimits, CodexAccount } from './legacy/server/codex-account-types'
import type { CodexSession } from './legacy/server/codex-rpc'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { readFile, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { CodexAccounts } from './legacy/server/codex-accounts'

export function limits(primary = 20, secondary = 40): AccountLimits {
  return {
    ordinaryUsageAllowed: true,
    rateLimits: {
      limitId: 'codex',
      limitName: 'Codex',
      primary: { usedPercent: primary, windowDurationMins: 300, resetsAt: Math.floor(Date.now() / 1000) + 7200 },
      secondary: { usedPercent: secondary, windowDurationMins: 10080, resetsAt: Math.floor(Date.now() / 1000) + 86400 },
    },
  }
}

export function credential(id: string) {
  return {
    tokens: {
      account_id: id,
      access_token: `synthetic-access-${id}`,
      refresh_token: `synthetic-refresh-${id}`,
      id_token: `header.${Buffer.from(JSON.stringify({ sub: id })).toString('base64url')}.signature`,
    },
  }
}

export async function accountFixture(ctx: Pick<Awaited<ReturnType<typeof fixture>>, 'service'>) {
  const responses = new Map<string, AccountLimits | Error>()
  const calls: string[] = []
  const redemptions: Array<{ id: string, params: { idempotencyKey: string, creditId?: string } }> = []
  const consume = new Map<string, (params: { idempotencyKey: string, creditId?: string }) => unknown | Promise<unknown>>()
  const session: CodexSession = async (home, operation) => {
    const filename = path.join(home, 'auth.json')
    const auth = JSON.parse(await readFile(filename, 'utf8'))
    const id = auth.tokens.account_id
    return operation({
      request: async <T>(method: string, params?: unknown) => {
        calls.push(method)
        if (method === 'account/read')
          return { account: { type: 'chatgpt', email: `${id}@example.test`, planType: 'plus' } } as T
        if (method === 'account/rateLimitResetCredit/consume') {
          const request = params as { idempotencyKey: string, creditId?: string }
          redemptions.push({ id, params: request })
          const handler = consume.get(id)
          if (!handler)
            throw new Error('Unexpected reset redemption')
          return await handler(request) as T
        }

        if (method !== 'account/rateLimits/read')
          throw new Error(`Unexpected mutating RPC: ${method}`)
        auth.tokens.refresh_token = `synthetic-rotated-${id}`
        await writeFile(filename, JSON.stringify(auth))
        const value = responses.get(id) ?? limits()
        if (value instanceof Error)
          throw value
        return value as T
      },
    })
  }

  const pool = new CodexAccounts(ctx.service.store, ctx.service.config, session)
  ctx.service.accounts = pool
  await pool.initialize()
  const seed = (name: string, value = limits()) => {
    const id = randomUUID()
    const account: CodexAccount = {
      id,
      name,
      enabled: true,
      email: `${name.toLowerCase().replace(/\s+/g, '.')}@example.test`,
      plan: 'plus',
      identity: null,
      createdAt: Date.now(),
      checkedAt: Date.now(),
      state: 'ready',
      error: '',
      limits: value,
      lastUsedAt: null,
      exhausted: null,
    }
    ctx.service.store.set('codex-accounts-enabled', true)
    ctx.service.store.put('codexAccounts', account)
    pool.vault.set(`codex-account:${id}`, credential(id))
    responses.set(id, value)
    return account
  }

  return {
    pool,
    seed,
    responses,
    calls,
    redemptions,
    consume,
  }
}
