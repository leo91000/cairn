import type { Provider } from '../../packages/contracts/accounts'
import type { Workspace } from './fixtures'
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'

// Synthetic accounts written straight into the native server's database. The fixture Codex
// app-server reports their usage; Claude accounts sign in through the fixture CLI instead.
export async function seedCodexAccount(workspace: Workspace, name: string, limits: unknown, options: { enabled?: boolean, maxConcurrentRuns?: number } = {}) {
  const id = randomUUID()
  const account = {
    id,
    provider: 'codex' as Provider,
    name,
    enabled: options.enabled ?? true,
    email: `${name.toLowerCase().replace(/\s+/g, '.')}@example.test`,
    plan: 'plus',
    identity: null,
    createdAt: Date.now(),
    checkedAt: null,
    state: 'ready',
    error: '',
    usage: null,
    lastUsedAt: null,
    exhausted: null,
    maxConcurrentRuns: options.maxConcurrentRuns ?? 4,
  }
  const { store, vault } = workspace.service
  store.put('agentAccounts', account)
  store.set('codex-accounts-enabled', true)
  vault.set(`codex-account:${id}`, credential(id))
  await workspace.setAccountUsage(id, limits)
  return account
}

/** Makes the next sign-in of this account follow the fixture's `mode`. */
export async function codexSignInMode(workspace: Workspace, id: string, mode: string) {
  const home = path.join(workspace.service.config.dataDir, 'account-login', id, '.codex')
  await mkdir(home, { recursive: true })
  await writeFile(path.join(home, 'fixture-login.json'), JSON.stringify({ mode }))
  return home
}

/** Connects a Claude account through the fixture CLI's code flow. */
export async function connectClaude(workspace: Workspace, name = 'Claude', code = 'fixture-code') {
  const started = await workspace.api('/api/accounts', 'POST', { provider: 'claude', name })
  for (let attempt = 0; attempt < 100; attempt++) {
    const { signIn } = await workspace.api('/api/accounts')
    if (signIn?.acceptsCode)
      break
    await new Promise(resolve => setTimeout(resolve, 50))
  }

  await workspace.api('/api/accounts/sign-in/code', 'POST', { code })
  for (let attempt = 0; attempt < 200; attempt++) {
    const { signIn } = await workspace.api('/api/accounts')
    if (signIn?.state !== 'pending')
      return { id: started.accountId as string, state: signIn?.state as string }
    await new Promise(resolve => setTimeout(resolve, 50))
  }

  throw new Error('Claude sign-in did not finish')
}

// Synthetic CLI responses, not persisted account-domain types.
export function limits(primary = 20, secondary = 40) {
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

function credential(id: string) {
  return {
    tokens: {
      account_id: id,
      access_token: `synthetic-access-${id}`,
      refresh_token: `synthetic-refresh-${id}`,
      id_token: `header.${Buffer.from(JSON.stringify({ sub: id })).toString('base64url')}.signature`,
    },
  }
}
