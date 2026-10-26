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
  return event('item.started', {
    item: {
      id: `tool-${serial}`,
      type: 'command_execution',
      command: 'ls',
      status: 'in_progress',
    },
  })
}

function reply(type: 'item.started' | 'item.completed', text: string) {
  return event(type, { item: { id: `message-${serial}`, type: 'agent_message', text } })
}

function agent(type: 'item.started' | 'item.updated', resumed = false) {
  return event(type, {
    item: {
      id: 'agent-1',
      type: 'subagent',
      status: 'in_progress',
      ...(resumed && { resumed }),
    },
  })
}

function status(text: string) {
  return { ...event('status'), text }
}

function runLog(events: RunEvent[]) {
  return activityEntries(events).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])
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

    expect(notices([first, reply('item.started', 'Here')])).toEqual([])
    // Starting a subagent is the agent's own tool starting.
    expect(notices([first, agent('item.started')])).toEqual([])

    // The run log keeps every retry, marking those work recovered from.
    const log = runLog([first, step()])
    expect(log.find(artifact => artifact.title.startsWith('Claude Code is retrying'))).toMatchObject({ statusLabel: 'Recovered', status: 'info' })
  })

  it('does not call retries recovered when the engine gives up on the request', () => {
    // Claude Code reports the last API error as a message, then the turn fails.
    const last = retry(10)
    const gaveUp = reply('item.completed', 'API Error: 529 Overloaded')
    const failed = event('turn.failed', { error: { message: 'API Error: 529 Overloaded' } })
    expect(notices([last, gaveUp]).map(notice => notice.title)).toEqual(['Claude Code is retrying the request · attempt 10/10'])
    expect(notices([last, gaveUp, failed]).map(notice => notice.title)).not.toContain('Claude Code is retrying the request · attempt 10/10')
    const retried = runLog([last, gaveUp, failed]).find(artifact => artifact.title.startsWith('Claude Code is retrying'))
    expect(retried?.statusLabel).not.toBe('Recovered')

    // A subagent's progress, or its resumption, does not mean the agent's own request went through.
    for (const progress of [agent('item.updated'), agent('item.started', true)])
      expect(notices([last, progress]).map(notice => notice.title)).toContain('Claude Code is retrying the request · attempt 10/10')
  })

  it('ends the engine retries with a run that stops before its turn completes', () => {
    for (const end of [status('cancelled'), status('interrupted'), status('failed'), event('runner.failed', { error: { message: 'Workspace sealing failed' } })]) {
      const last = retry(3)
      expect(notices([last, end]).map(notice => notice.title)).not.toContain('Claude Code is retrying the request · attempt 3/10')
      // A later execution's work does not recover the stopped run's retry.
      const retried = runLog([last, end, step()]).find(artifact => artifact.title.startsWith('Claude Code is retrying'))
      expect(retried?.statusLabel).not.toBe('Recovered')
    }
  })

  it('shows the usage limit until it no longer applies, its window resets or its run ends', () => {
    const warning = limit('allowed_warning')
    expect(notices([warning, step()]).map(notice => notice.title)).toEqual(['Approaching the 5-hour limit'])
    expect(notices([warning, limit('allowed')])).toEqual([])

    // A limit no longer applies once its window resets, even when no later state says so.
    const reset = (resetsAt: number) => event('provider.limit', {
      status: 'allowed_warning',
      window: 'five_hour',
      blocked: false,
      resetsAt,
    })
    expect(notices([reset(Date.now() - 60_000)])).toEqual([])
    expect(notices([reset(Date.now() + 3_600_000)]).map(notice => notice.title)).toEqual(['Approaching the 5-hour limit'])

    // A limit belongs to the execution that reported it: a later one reports its own.
    const attempt = { ...event('status', { attempt: true }), text: 'Preparing workspace' }
    for (const end of [[event('turn.completed'), status('succeeded')], [status('cancelled')], [attempt]])
      expect(notices([warning, step(), ...end]).map(notice => notice.title)).not.toContain('Approaching the 5-hour limit')

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

  it('tells the conversation a turn runs at Standard when Codex lacks the selected speed', () => {
    const fallback = {
      ...event('status', { speed: { requested: 'priority', applied: 'standard' } }),
      text: 'Codex doesn’t offer the selected speed for this model here. This turn runs at Standard speed.',
    }
    expect(notices([fallback, step()]).map(notice => [notice.title, notice.subtitle])).toEqual([
      ['This turn runs at Standard speed', 'Codex doesn’t list the selected speed for this model here, so Cairn asks for Standard'],
    ])
    expect(runLog([fallback])).toHaveLength(1)

    // Codex could not confirm the speed for this model: the turn may run at Standard.
    const unconfirmed = {
      ...event('status', { speed: { requested: 'priority', unconfirmed: true } }),
      text: 'Codex couldn’t confirm it offers the selected speed for this model here.',
    }
    expect(notices([unconfirmed, step()]).map(notice => [notice.title, notice.subtitle])).toEqual([
      ['Speed not confirmed for this turn', 'Codex couldn’t confirm it offers the selected speed for this model here; this turn may run at Standard'],
    ])

    // A later attempt, on another account, gets the speed after all.
    const confirmed = {
      ...event('status', { speed: { requested: 'priority', confirmed: true } }),
      text: 'Codex now offers the selected speed for this model here.',
    }
    expect(notices([fallback, confirmed]).map(notice => notice.title)).toEqual([
      'This turn runs at Standard speed',
      'Selected speed requested for this turn',
    ])
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

    // The runner repeats the failure without its reset, in wording the banner recognizes.
    const known = 'Claude AI usage limit reached'
    const reported = event('turn.failed', { error: { message: known, code: 'usage_limit_reached', resetsAt } })
    const repeated = event('turn.failed', { error: { message: known } })
    expect(chatQuota(run, [reported, repeated])).toMatchObject({ message: known, reset: limitReset(resetsAt) })
    // A new failure without a reset does not inherit an earlier one.
    const other = event('turn.failed', { error: { message: 'You\'ve hit your limit' } })
    expect(chatQuota(run, [reported, other])?.reset).toBeNull()
  })
})
