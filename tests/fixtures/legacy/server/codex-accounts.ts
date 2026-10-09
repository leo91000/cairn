import type { AccountLimits, CodexAccount, CodexAccountView } from './codex-account-types.ts'
import type { CodexSession } from './codex-rpc.ts'
import type { Config } from './config.ts'
import type { Run } from './contracts.ts'
import type { Store } from './store.ts'
import { Buffer } from 'node:buffer'
import { createHash, randomUUID } from 'node:crypto'
import {
  chmod,
  mkdir,
  readdir,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import path from 'node:path'
import { z } from 'zod'
import { CodexAccountResets } from './codex-account-resets.ts'
import { remainingUsage, usageBlocked, usageRecovered } from './codex-account-types.ts'
import { codexSession } from './codex-rpc.ts'
import { Connections } from './connections.ts'
import { AppError, requireValue } from './errors.ts'
import { McpVault } from './mcp-vault.ts'

const codexAccountInput = z.object({ name: z.string().trim().min(1).max(100), enabled: z.boolean().default(true) })
const authSchema = z.object({
  tokens: z.object({
    access_token: z.string().min(1),
    refresh_token: z.string().optional(),
    id_token: z.string().optional(),
    account_id: z.string().optional(),
  }).passthrough(),
}).passthrough()
const windowSchema = z.object({ usedPercent: z.number().finite().min(0), windowDurationMins: z.number().nullable(), resetsAt: z.number().nullable() })
const bucketSchema = z.object({
  limitId: z.string().nullable(),
  limitName: z.string().nullable().default(null),
  normalModelSlug: z.string().nullable().optional(),
  primary: windowSchema.nullable(),
  secondary: windowSchema.nullable(),
  rateLimitReachedType: z.string().nullable().optional(),
  spendControlReached: z.boolean().nullable().optional(),
})
const resetCreditsSchema = z.object({
  availableCount: z.number().int().min(0),
  credits: z.array(z.object({
    id: z.string().min(1),
    resetType: z.string(),
    status: z.string(),
    expiresAt: z.number().nullable(),
  })).nullable().optional(),
})
const limitsSchema = z.object({
  ordinaryUsageAllowed: z.boolean().nullable().optional(),
  accountId: z.string().nullable().optional(),
  rateLimits: bucketSchema,
  rateLimitsByLimitId: z.record(z.string(), bucketSchema).nullable().optional(),
  rateLimitResetCredits: resetCreditsSchema.nullable().optional(),
})

function authSubject(token?: string): string {
  try {
    return JSON.parse(Buffer.from(token?.split('.')[1] || '', 'base64url').toString()).sub || ''
  }
  catch {
    return ''
  }
}

const credentialKey = (id: string) => `codex-account:${id}`
const safeError = (error: unknown) => error instanceof AppError ? error.message : 'Unable to read this Codex account. Reconnect it and try again.'

export interface AccountLease {
  accountId: string
  runId: string
  home: string
  model: string
}

export class CodexAccounts {
  readonly vault: McpVault
  readonly leases = new Map<string, AccountLease>()
  readonly refreshing = new Map<string, Promise<void>>()
  readonly session: CodexSession
  private resets: CodexAccountResets
  private loginStarting = false
  private connecting = new Set<string>()
  private locks = new Map<string, Promise<unknown>>()
  private attempted = new Map<string, number>()
  timer: NodeJS.Timeout | undefined
  private initialized: Promise<void> | undefined
  private polling: Promise<void> | undefined
  private closing = false
  private login: {
    accountId: string
    controller: Connections
    home: string
    completion?: Promise<void>
  } | undefined

  constructor(readonly store: Store, readonly config: Config, session?: CodexSession) {
    this.vault = new McpVault(store, config.dataDir)
    this.session = session ?? codexSession(config)
    this.resets = new CodexAccountResets(store)
  }

  get(id: string) {
    return requireValue(this.store.get('codexAccounts', id), 'Codex account not found.')
  }

  managed() {
    return !!this.store.kv('codex-accounts-enabled')
  }

  view(account: CodexAccount): CodexAccountView {
    const { identity: _identity, ...safe } = account
    return {
      ...safe,
      remainingPercent: remainingUsage(account.limits),
      stale: !account.checkedAt || Date.now() - account.checkedAt > 90000,
      activeRunId: this.leases.get(account.id)?.runId ?? null,
    }
  }

  list() {
    return this.store.list('codexAccounts').sort((a, b) => a.createdAt - b.createdAt || a.name.localeCompare(b.name)).map(account => this.view(account))
  }

  private save(account: CodexAccount) {
    this.store.put('codexAccounts', account)
  }

  private async exclusive<T>(id: string, operation: () => Promise<T>): Promise<T> {
    const previous = this.locks.get(id) ?? Promise.resolve()
    const next = previous.catch(() => {}).then(operation)
    this.locks.set(id, next)
    try {
      return await next
    }
    finally {
      if (this.locks.get(id) === next)
        this.locks.delete(id)
    }
  }

  private newAccount(name: string) {
    const account: CodexAccount = {
      ...codexAccountInput.parse({ name }),
      id: randomUUID(),
      email: null,
      plan: null,
      identity: null,
      createdAt: Date.now(),
      checkedAt: null,
      state: 'pending',
      error: '',
      limits: null,
      lastUsedAt: null,
      exhausted: null,
    }
    this.save(account)
    this.store.set('codex-accounts-enabled', true)
    return account
  }

  private async capture(id: string, home: string) {
    const auth = authSchema.parse(JSON.parse(await readFile(path.join(home, 'auth.json'), 'utf8')))
    const previous = this.vault.get<z.infer<typeof authSchema>>(credentialKey(id))
    if ((previous?.tokens.account_id && auth.tokens.account_id !== previous.tokens.account_id) || (authSubject(previous?.tokens.id_token) && authSubject(previous?.tokens.id_token) !== authSubject(auth.tokens.id_token)))
      throw new AppError(409, 'Sign-in belongs to a different account. Add it as a new account instead.')
    this.vault.set(credentialKey(id), auth)
    await chmod(path.join(home, 'auth.json'), 0o600)
  }

  private recovering(id: string) {
    return this.store.active().some(run => run.recoveryPending && run.codexAccountId === id)
  }

  async recoverRun(run: Run) {
    for (const relative of ['codex', 'home/.codex']) {
      const home = path.join(this.config.dataDir, 'runs', run.id, relative)
      if (run.codexAccountId && this.store.get('codexAccounts', run.codexAccountId))
        await this.capture(run.codexAccountId, home).catch(() => {})
      await rm(path.join(home, 'auth.json'), { force: true })
    }

    for (const [id, lease] of this.leases) {
      if (lease.runId === run.id)
        this.leases.delete(id)
    }
  }

  redactions(id: string) {
    const auth = this.vault.get<z.infer<typeof authSchema>>(credentialKey(id))
    return [auth?.tokens.access_token, auth?.tokens.refresh_token, auth?.tokens.id_token].filter((value): value is string => !!value)
  }

  private async materialize(id: string, home: string) {
    const auth = requireValue(this.vault.get(credentialKey(id)), 'Reconnect this Codex account before using it.')
    await mkdir(home, { recursive: true, mode: 0o700 })
    await writeFile(path.join(home, 'auth.json'), JSON.stringify(auth), { mode: 0o600 })
    await writeFile(path.join(home, 'config.toml'), 'cli_auth_credentials_store = "file"\nforced_login_method = "chatgpt"\n', { mode: 0o600 })
  }

  initialize() {
    this.initialized ??= (async () => {
      // Recover short-lived monitor/login credentials before run credentials: a run can
      // have refreshed them again after a monitor completed but failed to clean up.
      for (const account of this.store.list('codexAccounts')) {
        for (const relative of ['codex-monitor', 'codex-login']) {
          const directory = path.join(this.config.dataDir, relative, account.id)
          await this.capture(account.id, relative === 'codex-login' ? path.join(directory, '.codex') : directory).catch(() => {})
          await rm(directory, { recursive: true, force: true })
        }
      }

      // Recover rotated credentials from a process that stopped before releasing its lease.
      for (const id of await readdir(path.join(this.config.dataDir, 'runs')).catch(() => [])) {
        if (!z.uuid().safeParse(id).success)
          continue
        const run = this.store.run(id)
        if (run?.recoveryPending)
          continue
        for (const relative of ['codex', 'home/.codex']) {
          const home = path.join(this.config.dataDir, 'runs', id, relative)
          if (run?.codexAccountId && this.store.get('codexAccounts', run.codexAccountId))
            await this.capture(run.codexAccountId, home).catch(() => {})
          await rm(path.join(home, 'auth.json'), { force: true })
        }
      }

      if (this.managed())
        return
      const legacy = path.join(this.config.home, '.codex')
      const auth = await readFile(path.join(legacy, 'auth.json'), 'utf8').catch(() => null)
      if (!auth)
        return
      try {
        if (!authSchema.safeParse(JSON.parse(auth)).success)
          return
      }
      catch {
        return
      }

      const account = this.newAccount('Primary account')
      await this.capture(account.id, legacy)
      this.store.audit('codex.account.imported', { id: account.id })
    })()
    return this.initialized
  }

  start() {
    this.timer = setInterval(() => {
      void this.poll(true).catch(() => {})
    }, 15000)
    this.timer.unref()
    void this.poll().catch(() => {})
  }

  async poll(onlyDue = false) {
    if (this.closing)
      return
    this.polling ??= (async () => {
      await this.initialize()
      const accounts = this.store.list('codexAccounts').filter(account => account.state !== 'pending')
      const imported = this.store.list('codexAccounts').filter(account => account.state === 'pending' && this.vault.get(credentialKey(account.id)))
      const ids = [...accounts, ...imported].filter(account => !onlyDue || Date.now() - (this.attempted.get(account.id) ?? 0) >= this.resets.pollInterval(account, this.leases.get(account.id)?.model ?? account.exhausted?.model ?? '')).map(account => account.id)
      for (let index = 0; index < ids.length; index += 2)
        await Promise.all(ids.slice(index, index + 2).map(id => this.refresh(id)))
    })().finally(() => {
      this.polling = undefined
    })
    return this.polling
  }

  async refresh(id: string) {
    if (this.refreshing.has(id))
      return this.refreshing.get(id)
    const operation = this.exclusive(id, () => this.readUsage(id)).finally(() => this.refreshing.delete(id))
    this.refreshing.set(id, operation)
    return operation
  }

  private async readUsage(id: string) {
    if (!this.store.get('codexAccounts', id) || (this.login?.accountId === id && this.login.controller.child))
      return
    if (this.recovering(id))
      return
    this.attempted.set(id, Date.now())
    const lease = this.leases.get(id)
    const home = lease?.home ?? path.join(this.config.dataDir, 'codex-monitor', id)
    try {
      if (!lease)
        await this.materialize(id, home)
      await this.session(home, async (rpc) => {
        const identity = await rpc.request<{ account: { type: string, email: string | null, planType: string } | null }>('account/read', { refreshToken: false })
        if (identity.account?.type !== 'chatgpt')
          throw new AppError(400, 'Connect a ChatGPT subscription account. API keys are not supported here.')
        const auth = authSchema.parse(JSON.parse(await readFile(path.join(home, 'auth.json'), 'utf8')))
        const readLimits = async (): Promise<AccountLimits> => {
          const limits = limitsSchema.parse(await rpc.request('account/rateLimits/read'))
          if (limits.accountId && auth.tokens.account_id && limits.accountId !== auth.tokens.account_id)
            throw new AppError(409, 'Codex returned usage for a different account. Reconnect this account.')
          return limits
        }

        let limits = await readLimits()
        let account = this.get(id)
        const subject = authSubject(auth.tokens.id_token) || identity.account.email || ''
        if (!subject)
          throw new AppError(400, 'Codex did not return an account identity. Reconnect this account.')
        const fingerprint = createHash('sha256').update(`${auth.tokens.account_id || limits.accountId || ''}:${subject}`).digest('hex')
        if (account.identity && account.identity !== fingerprint)
          throw new AppError(409, 'Sign-in belongs to a different account. Add it as a new account instead.')
        const duplicate = this.store.list('codexAccounts').find(other => other.id !== id && other.identity === fingerprint)
        if (duplicate)
          throw new AppError(409, 'This account is already connected. Reconnect the existing account instead.')
        account = {
          ...account,
          identity: fingerprint,
          email: identity.account.email,
          plan: identity.account.planType,
          state: 'ready',
          checkedAt: Date.now(),
          error: '',
          limits,
        }
        if (account.exhausted && usageRecovered(account.exhausted.limits, limits, account.exhausted.model))
          account.exhausted = null
        this.save(account)
        const reset = await this.resets.refresh(account, lease?.model ?? account.exhausted?.model ?? '', rpc, readLimits)
        limits = reset.limits
        account = { ...this.get(id), limits, resetError: reset.resetError }
        // A timestamp alone does not restore capacity: require a fresh usage increase.
        if (account.exhausted) {
          const recovered = (reset.resetConfirmed && !usageBlocked(limits, account.exhausted.model) && (remainingUsage(limits, account.exhausted.model) ?? 0) > 0) || usageRecovered(account.exhausted.limits, limits, account.exhausted.model)
          account = { ...account, exhausted: recovered ? null : { ...account.exhausted, limits } }
        }

        this.save(account)
      })
      await this.capture(id, home)
    }
    catch (error) {
      this.save({ ...this.get(id), state: 'error', error: safeError(error) })
    }
    finally {
      if (!lease) {
        await this.capture(id, home).catch(() => {})
        await rm(home, { recursive: true, force: true })
      }
    }
  }

  update(id: string, input: unknown) {
    if (this.login?.accountId === id && (this.login.controller.child || this.login.completion))
      throw new AppError(409, 'Finish or cancel sign-in before editing this account.')
    const updated = { ...this.get(id), ...codexAccountInput.parse(input) }
    this.save(updated)
    return this.view(updated)
  }

  async remove(id: string) {
    this.get(id)
    if (this.recovering(id) || this.leases.has(id) || (this.login?.accountId === id && (this.login.controller.child || this.login.completion)))
      throw new AppError(409, 'Wait for this account’s run or sign-in to finish before removing it.')
    await this.exclusive(id, async () => {
      if (this.recovering(id) || this.leases.has(id) || this.connecting.has(id))
        throw new AppError(409, 'This account is in use.')
      this.vault.delete(credentialKey(id))
      this.resets.remove(id)
      this.store.remove('codexAccounts', id)
      this.store.audit('codex.account.removed', { id })
    })
  }

  async acquire(runId: string, model = ''): Promise<AccountLease | null> {
    await this.initialize()
    if (!this.managed())
      return null
    const stale = this.store.list('codexAccounts').filter(account => account.enabled && Date.now() - (this.attempted.get(account.id) ?? 0) > 60000 && (!account.checkedAt || Date.now() - account.checkedAt > 60000))
    await Promise.all(stale.map(account => this.refresh(account.id)))
    const candidates = this.store.list('codexAccounts').filter(account => account.enabled && !account.exhausted && account.state === 'ready' && account.checkedAt && Date.now() - account.checkedAt <= 90000 && !this.recovering(account.id) && !this.leases.has(account.id) && !this.connecting.has(account.id) && !(this.login?.accountId === account.id && (this.login.controller.child || this.login.completion)) && account.limits && !usageBlocked(account.limits, model) && (remainingUsage(account.limits, model) ?? 0) > 0).sort((a, b) => remainingUsage(b.limits, model)! - remainingUsage(a.limits, model)! || (a.lastUsedAt ?? 0) - (b.lastUsedAt ?? 0) || a.id.localeCompare(b.id))
    const account = candidates[0]
    if (!account)
      throw new AppError(409, 'Waiting for a Codex account with available usage. Usage is checked every minute.')
    const lease = {
      accountId: account.id,
      runId,
      model,
      home: path.join(this.config.dataDir, 'runs', runId, 'codex'),
    }
    this.leases.set(account.id, lease)
    try {
      await this.exclusive(account.id, () => this.materialize(account.id, lease.home))
      this.save({ ...this.get(account.id), lastUsedAt: Date.now() })
      return lease
    }
    catch (error) {
      this.leases.delete(account.id)
      throw error
    }
  }

  markExhausted(id: string, model: string) {
    const account = this.get(id)
    this.save({ ...account, exhausted: { at: Date.now(), model, limits: account.limits } })
    this.store.audit('codex.account.exhausted', { id })
  }

  async relocate(lease: AccountLease, home: string) {
    await this.exclusive(lease.accountId, async () => {
      await this.capture(lease.accountId, lease.home)
      const previousHome = lease.home
      if (previousHome === home)
        return
      await this.materialize(lease.accountId, home)
      lease.home = home
      await rm(path.join(previousHome, 'auth.json'), { force: true })
    })
  }

  async release(lease: AccountLease) {
    await this.exclusive(lease.accountId, async () => {
      try {
        await this.capture(lease.accountId, lease.home)
      }
      finally {
        await rm(path.join(lease.home, 'auth.json'), { force: true })
        this.leases.delete(lease.accountId)
      }
    })
  }

  flow() {
    if (!this.login)
      return null
    return { ...this.login.controller.flow, accountId: this.login.accountId }
  }

  async startLogin(name: string, id?: string) {
    if (this.loginStarting)
      throw new AppError(409, 'A Codex sign-in is already in progress.')
    this.loginStarting = true
    try {
      return await this.beginLogin(name, id)
    }
    finally {
      this.loginStarting = false
      this.connecting.clear()
    }
  }

  private async beginLogin(name: string, id?: string) {
    await this.initialize()
    if (this.login?.controller.child || this.login?.completion)
      throw new AppError(409, 'A Codex sign-in is already in progress.')
    if (id && (this.recovering(id) || this.leases.has(id)))
      throw new AppError(409, 'Wait for this account’s run to finish before reconnecting it.')
    if (!id && this.store.list('codexAccounts').length >= 10)
      throw new AppError(400, 'A maximum of ten Codex accounts can be connected.')
    const account = id ? this.get(id) : this.newAccount(name)
    this.connecting.add(account.id)
    await this.refreshing.get(account.id)
    const home = path.join(this.config.dataDir, 'codex-login', account.id)
    await mkdir(path.join(home, '.codex'), { recursive: true, mode: 0o700 })
    await writeFile(path.join(home, '.codex', 'config.toml'), 'cli_auth_credentials_store = "file"\nforced_login_method = "chatgpt"\n', { mode: 0o600 })
    const controller = new Connections({ ...this.config, home })
    const login = {
      accountId: account.id,
      controller,
      home,
      completion: undefined as Promise<void> | undefined,
    }
    this.login = login
    controller.start('codex')
    controller.child!.once('close', () => {
      login.completion = (async () => {
        try {
          if (controller.flow?.state !== 'complete')
            return
          controller.flow.state = 'pending'
          const previous = this.vault.get(credentialKey(account.id))
          await this.capture(account.id, path.join(home, '.codex'))
          await this.refresh(account.id)
          if (this.get(account.id).state !== 'ready') {
            if (previous)
              this.vault.set(credentialKey(account.id), previous)
            else
              this.vault.delete(credentialKey(account.id))
            throw new AppError(400, this.get(account.id).error)
          }

          controller.flow.state = 'complete'
        }
        catch (error) {
          if (controller.flow) {
            controller.flow.state = 'failed'
            controller.flow.error = safeError(error)
          }
        }
        finally {
          await rm(home, { recursive: true, force: true })
          login.completion = undefined
        }
      })()
    })
    return this.flow()
  }

  async cancelLogin() {
    const login = this.login
    if (!login)
      return
    const child = login.controller.child
    login.controller.cancel()
    if (child && child.exitCode === null && child.signalCode === null) {
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          child.kill('SIGKILL')
          resolve()
        }, 2000)
        child.once('close', () => {
          clearTimeout(timer)
          resolve()
        })
      })
    }

    await login.completion
    await rm(login.home, { recursive: true, force: true })
    this.login = undefined
  }

  async close() {
    this.closing = true
    clearInterval(this.timer)
    await this.cancelLogin()
    await this.polling
    await Promise.all(this.refreshing.values())
  }
}
