import type { Provider } from '../../packages/contracts/accounts'
import type { AccountLimits } from '../fixtures/legacy/server/codex-account-types'
import type { Workspace } from './fixtures'
import { randomUUID } from 'node:crypto'
import { mkdir, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { credential } from '../fixtures/codex-account-fixture'
import { McpVault } from '../fixtures/legacy/server/mcp-vault'

// Synthetic accounts written straight into the native server's database. The fixture Codex
// app-server reports their usage; Claude accounts sign in through the fixture CLI instead.
export async function seedCodexAccount(workspace: Workspace, name: string, limits: AccountLimits, options: { enabled?: boolean, maxConcurrentRuns?: number } = {}) {
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
  const { store, config } = workspace.service
  store.db.prepare('INSERT OR REPLACE INTO records(id,kind,data,updated_at) VALUES(?,?,?,?)').run(id, 'agentAccounts', JSON.stringify(account), Date.now())
  store.set('codex-accounts-enabled', true)
  new McpVault(store, config.dataDir).set(`codex-account:${id}`, credential(id))
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
