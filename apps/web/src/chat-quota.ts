import type { Run, RunEvent } from '../../../packages/contracts/contracts'
import type { ActivityEntry } from './activity'
import { providers } from '../../../packages/contracts/accounts'
import { activityPayload } from './activity'

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
  const failure = current.findLast(event => quotaFailure(event))
  const failureMessage = failure ? quotaFailure(failure) : ''
  const waiting = ['running', 'queued'].includes(run.status) && /^(?:Usage exhausted\.|Waiting for a .+ account with available usage\.)/i.test(run.accountWaitReason ?? '')
  const savedQuota = run.status === 'failed' && (isQuotaMessage(run.error) || (!!failureMessage && failureMessage === run.error))
  if (!waiting && !savedQuota)
    return null
  const message = failureMessage || (isQuotaMessage(run.error) ? run.error! : '')

  return {
    provider: providers[run.snapshot.agent.provider ?? 'codex'].label,
    message,
    spend: /\bspend limit\b/i.test(message),
    // Keep the announced zone and wording: a time without a date is not a deadline.
    reset: /\bresets\b(.*)$/i.exec(message)?.[1]?.trim().replace(/\.$/, '') || null,
    // Pausing the follow-up queue does not pause an execution's capacity wait.
    automatic: waiting,
  }
}

/** Confirmed quota interruptions are transient chat bookkeeping; run diagnostics retain them. */
export function quotaHiddenEvents(events: RunEvent[]) {
  const hidden = new Set<string>()
  const turns = new Map<string, {
    exhausted: boolean
    legacyTool: boolean
    causes: Set<string>
    messages: Map<string, { text: string, ids: string[] }>
  }>()
  for (const event of events) {
    if (!turns.has(event.runId) || ['turn.started', 'chat.user'].includes(event.type)) {
      turns.set(event.runId, {
        exhausted: false,
        legacyTool: false,
        causes: new Set(),
        messages: new Map(),
      })
    }

    const turn = turns.get(event.runId)!
    const id = `${event.runId}:${event.id}`
    const data = activityPayload(event)
    const item = data.item as Record<string, unknown> | undefined
    const legacy = !event.payload && !Object.keys(data).length
    const legacyMessage = legacy && event.type === 'item.completed' && !turn.legacyTool
    if (legacy && event.type === 'item.started')
      turn.legacyTool = true
    if (legacy && event.type === 'item.completed')
      turn.legacyTool = false
    if ((item?.type === 'agent_message' && typeof item.text === 'string') || legacyMessage) {
      const key = typeof item?.id === 'string' ? item.id : id
      const message = turn.messages.get(key) ?? { text: '', ids: [] }
      message.text = typeof item?.text === 'string' ? item.text : event.text
      message.ids.push(id)
      turn.messages.set(key, message)
      if (turn.exhausted && (isQuotaMessage(message.text) || turn.causes.has(message.text)))
        message.ids.forEach(id => hidden.add(id))
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
