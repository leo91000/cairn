import type { AccountLimits, CodexAccount } from './codex-account-types.ts'
import type { CodexRpc } from './codex-rpc.ts'
import type { Store } from './store.ts'
import { randomUUID } from 'node:crypto'
import { z } from 'zod'
import { remainingUsage, usageBlocked } from './codex-account-types.ts'

export const RESET_REMAINING_PERCENT = 2
const outcomeSchema = z.object({ outcome: z.enum(['reset', 'alreadyRedeemed', 'nothingToReset', 'noCredit']) })
const resetKey = (id: string) => `codex-reset:${id}`

interface ResetAttempt {
  params: { idempotencyKey: string, creditId?: string }
  model: string
  confirmed: boolean
}

// Called under the account's exclusive lock, after its identity has been verified.
export class CodexAccountResets {
  constructor(private store: Store) {}

  remove(id: string) {
    this.store.delete(resetKey(id))
  }

  pollInterval(account: CodexAccount, model: string) {
    const remaining = remainingUsage(account.limits, model)
    const pending = !!this.store.kv(resetKey(account.id))
    const hasCredits = (account.limits?.rateLimitResetCredits?.availableCount ?? 0) > 0
    const needsAttention = account.exhausted || pending || (remaining !== null && remaining <= 10)
    return account.enabled && needsAttention && (hasCredits || pending) ? 15000 : 60000
  }

  async refresh(account: CodexAccount, model: string, rpc: CodexRpc, readLimits: () => Promise<AccountLimits>) {
    let limits = account.limits!
    const key = resetKey(account.id)
    let attempt = this.store.kv<ResetAttempt>(key)
    const restored = (value: AccountLimits, targetModel: string) => !usageBlocked(value, targetModel) && (remainingUsage(value, targetModel) ?? 0) > RESET_REMAINING_PERCENT
    if (attempt && !account.exhausted && restored(limits, attempt.model)) {
      // Fresh recovered capacity closes this depletion cycle even if its reply was lost.
      this.remove(account.id)
      return { limits, resetError: '', resetConfirmed: attempt.confirmed }
    }

    if (attempt?.confirmed) {
      // Never spend another credit while the backend still reports the old low window.
      if (!restored(limits, attempt.model))
        return { limits, resetError: 'Banked reset redeemed; waiting for refreshed capacity.', resetConfirmed: false }
      this.remove(account.id)
      return { limits, resetError: '', resetConfirmed: true }
    }

    const remaining = remainingUsage(limits, model)
    if (!account.enabled || (!account.exhausted && (remaining === null || remaining > RESET_REMAINING_PERCENT)))
      return { limits, resetError: '', resetConfirmed: false }
    if (!attempt && !(limits.rateLimitResetCredits && limits.rateLimitResetCredits.availableCount > 0))
      return { limits, resetError: '', resetConfirmed: false }

    if (!attempt) {
      const credits = limits.rateLimitResetCredits?.credits ?? []
      const credit = credits.filter(credit => credit.status === 'available' && credit.resetType === 'codexRateLimits' && (credit.expiresAt === null || credit.expiresAt * 1000 > Date.now()))
        .sort((a, b) => (a.expiresAt ?? Infinity) - (b.expiresAt ?? Infinity))[0]
      attempt = { params: { idempotencyKey: randomUUID(), ...(credit ? { creditId: credit.id } : {}) }, model, confirmed: false }
      // Persist before sending: a lost reply or worker restart must reuse this exact request.
      this.store.set(key, attempt)
    }

    try {
      const { outcome } = outcomeSchema.parse(await rpc.request('account/rateLimitResetCredit/consume', attempt.params))
      if (outcome === 'nothingToReset' || outcome === 'noCredit') {
        this.remove(account.id)
        return { limits, resetError: outcome === 'nothingToReset' ? 'Banked reset is not eligible yet; checking again automatically.' : 'No banked reset available; waiting for capacity or another account.', resetConfirmed: false }
      }

      attempt.confirmed = true
      this.store.set(key, attempt)
      this.store.audit('codex.account.reset', { id: account.id, outcome })
      limits = await readLimits()
      const resetConfirmed = restored(limits, attempt.model)
      if (resetConfirmed)
        this.remove(account.id)
      return { limits, resetError: resetConfirmed ? '' : 'Banked reset redeemed; waiting for refreshed capacity.', resetConfirmed }
    }
    catch {
      // Reset failures must not make otherwise usable capacity unavailable or stop a run.
      return { limits, resetError: 'Unable to confirm banked reset. Retrying automatically without spending another reset.', resetConfirmed: false }
    }
  }
}
