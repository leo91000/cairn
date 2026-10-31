import type { Run } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import { providers } from '../../../packages/contracts/accounts'
import { limitReset } from './engine-status'

const quotaMessage = /^(?:you['’]ve hit your (?:(?:usage|session|weekly|spend|individual spend) )?limit|you have hit your (?:usage )?limit|usage limit (?:has been )?(?:reached|exceeded)|claude ai usage limit reached|(?:5-hour|session|weekly|opus) limit reached)\b/i

export function isQuotaMessage(message: string | null | undefined) {
  return quotaMessage.test(message?.trim() ?? '')
}

export interface ChatQuota {
  provider: string
  message: string
  spend: boolean
  reset: string | null
  automatic: boolean
}

/** A live account wait that recovers automatically, without reconnecting an account. */
export function isQuotaWait(run: { status: string, accountWaitReason?: string | null, accountRequired?: string | null }) {
  return ['running', 'queued'].includes(run.status) && !run.accountRequired
    && /^(?:Usage exhausted\.|Waiting for a .+ account with available usage\.)/i.test(run.accountWaitReason ?? '')
}

/** Only a current failure or the manager's explicit capacity wait can promise recovery. */
export function chatQuota(run: Run | null | undefined, cards: TimelineEntry[]): ChatQuota | null {
  if (!run)
    return null
  // The server marks the current turn's usage-limit failure, with the reset the engine gave.
  const current = cards.findLast(card => card.runId === run.id && card.content.kind === 'notice' && card.content.notice.quota?.current)
  const quota = current?.content.kind === 'notice' ? current.content.notice.quota : undefined
  const failureMessage = quota?.message ?? ''
  const failureReset = quota?.resetsAt ?? null

  const active = ['running', 'queued'].includes(run.status)
  const exhausted = active && /^Usage exhausted\./i.test(run.accountWaitReason ?? '')
  const admission = active && /^Waiting for a .+ account with available usage\./i.test(run.accountWaitReason ?? '')
  const waiting = exhausted || admission
  const savedQuota = run.status === 'failed' && (isQuotaMessage(run.error) || (!!failureMessage && failureMessage === run.error))
  if (!waiting && !savedQuota)
    return null
  // Admission has no current agent failure; saved failures have an authoritative error.
  const message = savedQuota ? run.error! : exhausted ? failureMessage : ''

  return {
    provider: providers[run.snapshot.agent.provider ?? 'codex'].label,
    message,
    spend: /\bspend limit\b/i.test(message),
    // Keep the announced zone and wording: a time without a date is not a deadline.
    reset: /\bresets\b(.*)$/i.exec(message)?.[1]?.trim().replace(/\.$/, '') || (message === failureMessage && limitReset(failureReset)) || null,
    // Pausing the follow-up queue does not pause an execution's capacity wait.
    automatic: waiting,
  }
}
