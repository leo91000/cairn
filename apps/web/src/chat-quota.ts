import type { Run, RunEvent } from '../../../packages/contracts/contracts'
import type { ActivityEntry } from './activity'
import { providers } from '../../../packages/contracts/accounts'
import { activityPayload, failureReason } from './activity'
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

function quotaFailure(event: RunEvent) {
  if (!['turn.failed', 'error'].includes(event.type))
    return ''
  const data = activityPayload(event)
  if (data.willRetry === true)
    return ''
  const error = event.type === 'turn.failed' ? data.error as Record<string, unknown> | undefined : data
  const message = error?.message
  const text = typeof message === 'string' ? message : event.text
  const exhausted = error?.code === 'usage_limit_reached' || error?.codexErrorInfo === 'usageLimitExceeded' || isQuotaMessage(text)
  return exhausted ? text || 'Usage limit reached' : ''
}

/** Only a current failure or the manager's explicit capacity wait can promise recovery. */
export function chatQuota(run: Run | null | undefined, events: RunEvent[]): ChatQuota | null {
  if (!run)
    return null
  const current = events.filter(event => event.runId === run.id)
  let failureMessage = ''
  // When the engine reported the limit, it said when the limit resets.
  let failureReset: unknown
  for (const event of current) {
    const turnBoundary = ['chat.user', 'turn.started', 'turn.completed'].includes(event.type)
    const executionBoundary = event.type === 'status' && ['queued', 'succeeded', 'cancelled', 'interrupted'].includes(event.text)
    if (turnBoundary || executionBoundary) {
      failureMessage = ''
      failureReset = undefined
    }

    const failure = quotaFailure(event)
    if (failure) {
      // The runner repeats the engine's failure without its reset.
      const reset = (activityPayload(event).error as Record<string, unknown> | undefined)?.resetsAt
      failureReset = reset ?? (failure === failureMessage ? failureReset : undefined)
      failureMessage = failure
    }
  }

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

/** Confirmed quota interruptions and replies echoing a failure are transient chat bookkeeping; run diagnostics retain them. */
export function quotaHiddenEvents(events: RunEvent[]) {
  const hidden = new Set<string>()
  const turns = new Map<string, {
    exhausted: boolean
    causes: Set<string>
    failures: Set<string>
    messages: Map<string, { text: string, ids: string[] }>
  }>()
  for (const event of events) {
    if (!turns.has(event.runId) || ['turn.started', 'chat.user'].includes(event.type)) {
      turns.set(event.runId, {
        exhausted: false,
        causes: new Set(),
        failures: new Set(),
        messages: new Map(),
      })
    }

    const turn = turns.get(event.runId)!
    const id = `${event.runId}:${event.id}`
    const data = activityPayload(event)
    const item = data.item as Record<string, unknown> | undefined
    if (item?.type === 'agent_message' && typeof item.text === 'string') {
      const key = typeof item?.id === 'string' ? item.id : id
      const message = turn.messages.get(key) ?? { text: '', ids: [] }
      message.text = typeof item?.text === 'string' ? item.text : event.text
      message.ids.push(id)
      turn.messages.set(key, message)
      if ((turn.exhausted && (isQuotaMessage(message.text) || turn.causes.has(message.text))) || turn.failures.has(message.text.trim()))
        message.ids.forEach(id => hidden.add(id))
    }

    // An agent may also say its failure as a reply, as Claude Code does for API errors: the
    // failure card already shows it.
    const reason = failureReason(event)?.trim()
    if (reason && reason !== 'Conversation interrupted.') {
      turn.failures.add(reason)
      for (const message of turn.messages.values()) {
        if (message.text.trim() === reason)
          message.ids.forEach(id => hidden.add(id))
      }
    }

    const cause = quotaFailure(event)
    if (cause) {
      turn.exhausted = true
      turn.causes.add(cause)
      hidden.add(id)
      for (const message of turn.messages.values()) {
        if (isQuotaMessage(message.text) || turn.causes.has(message.text))
          message.ids.forEach(id => hidden.add(id))
      }
    }

    if (turn.exhausted && event.type === 'status' && event.text === 'failed')
      hidden.add(id)
  }

  return hidden
}

/** Process lifecycle events first, then hide quota echoes without changing tools or subagents. */
export function quotaEntries(entries: ActivityEntry[], events: RunEvent[]) {
  const hidden = quotaHiddenEvents(events)
  return entries.filter((entry) => {
    if (entry.kind === 'message')
      return !hidden.has(entry.sourceId ?? '')
    if (entry.kind === 'notice')
      return !entry.artifact.sourceIds?.some(id => hidden.has(id))
    return true
  })
}
