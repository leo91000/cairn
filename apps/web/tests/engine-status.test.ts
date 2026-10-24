import type { Run, RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import { chatQuota } from '../src/chat-quota'
import {
  engineRetryDetail,
  engineRetryTitle,
  limitReset,
  usageLimitDetail,
  usageLimitTitle,
} from '../src/engine-status'

let serial = 0

function event(type: string, payload: Record<string, unknown> = {}): RunEvent {
  const id = ++serial
  return {
    id,
    runId: 'run',
    createdAt: id * 1000,
    type,
    text: JSON.stringify({ type, ...payload }),
    payload: { type, ...payload },
  }
}

function retry(attempt: number) {
  return event('engine.retry', {
    attempt,
    maxRetries: 10,
    delayMs: 4000,
    status: 529,
    reason: 'overloaded',
  })
}

const limit = (status: string, blocked = false) => event('provider.limit', { status, window: 'five_hour', blocked })
const notice = (level: string) => event('engine.notice', { level, message: 'Context is almost full.' })

function step() {
  return event('item.completed', {
    item: {
      id: `tool-${serial}`,
      type: 'command_execution',
      command: 'ls',
      exit_code: 0,
    },
  })
}

function notices(events: RunEvent[]) {
  return activityEntries(events, true).flatMap(entry => entry.kind === 'notice' ? [entry.artifact] : [])
}

describe('engine status', () => {
  it('describes the engine retries and usage limits without calling them failures', () => {
    expect(engineRetryTitle({ attempt: 2, maxRetries: 10 })).toBe('Claude Code is retrying the request · attempt 2/10')
    expect(engineRetryDetail({ reason: 'overloaded', status: 529, delayMs: 4000 })).toBe('Servers overloaded · HTTP 529 · next attempt after 4 s')
    expect(engineRetryDetail({ noResponse: { waitedMs: 30000 }, delayMs: 60000 })).toBe('No response from the API · next attempt after 60 s')
    expect(usageLimitTitle({ status: 'allowed_warning', window: 'five_hour' })).toBe('Approaching the 5-hour limit')
    expect(usageLimitTitle({ status: 'rejected', window: 'seven_day', blocked: true })).toBe('Weekly limit reached')
    expect(usageLimitTitle({ status: 'rejected', blocked: false })).toBe('Usage limit reached · continuing on extra usage')
    expect(usageLimitTitle({ status: 'allowed' })).toBe('Usage limit no longer applies')
    expect(usageLimitDetail({})).toBe('')
    const reset = new Date(2030, 0, 1, 12, 0).getTime()
    expect(limitReset(reset, reset)).toBe(new Date(reset).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }))
    expect(limitReset(reset, reset - 86_400_000)).toContain(new Date(reset).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }))
    expect(limitReset(reset, reset - 86_400_000).length).toBeGreaterThan(limitReset(reset, reset).length)
  })

  it('shows the latest engine retry until work continues, never as an error', () => {
    const first = retry(1)
    const second = retry(2)
    const shown = notices([first, second])
    expect(shown).toHaveLength(1)
    expect(shown[0]).toMatchObject({ title: 'Claude Code is retrying the request · attempt 2/10', status: 'info' })
    expect(notices([first, second, step()])).toEqual([])
    expect(notices([first, event('turn.completed')])).toEqual([])

    // The run log keeps every retry, marking those work recovered from.
    const log = activityEntries([first, step()]).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])
    expect(log.find(artifact => artifact.title.startsWith('Claude Code is retrying'))).toMatchObject({ statusLabel: 'Recovered', status: 'info' })
  })

  it('shows the usage limit until it no longer applies or the turn fails', () => {
    const warning = limit('allowed_warning')
    expect(notices([warning, step()]).map(notice => notice.title)).toEqual(['Approaching the 5-hour limit'])
    expect(notices([warning, limit('allowed')])).toEqual([])

    const blocked = limit('rejected', true)
    expect(notices([blocked]).map(notice => notice.title)).toEqual(['5-hour limit reached'])
    const failed = event('turn.failed', { error: { message: 'Your usage allowance is used up for now.', code: 'usage_limit_reached' } })
    expect(notices([blocked, failed]).map(notice => notice.title)).not.toContain('5-hour limit reached')
  })

  it('keeps informational notices in the run log', () => {
    const events = [notice('warning'), notice('info'), notice('notice')]
    expect(notices(events).map(notice => notice.title)).toEqual(['Claude Code warning'])
    expect(activityEntries(events).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])).toHaveLength(3)
  })

  it('gives the usage banner the reset the engine reported', () => {
    const resetsAt = new Date(2030, 0, 1, 12, 0).getTime()
    const message = 'Your usage allowance is used up for now.'
    const run = {
      id: 'run',
      status: 'running',
      accountWaitReason: 'Usage exhausted. Waiting for an available Claude Code account to resume.',
      snapshot: { agent: { provider: 'claude' } },
    } as Run
    const failed = event('turn.failed', { error: { message, code: 'usage_limit_reached', resetsAt } })
    expect(chatQuota(run, [failed])).toMatchObject({ message, reset: limitReset(resetsAt) })
  })
})
