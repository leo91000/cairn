import type { Run } from '../../../packages/contracts/contracts'
import type { TimelineNotice } from '../../../packages/contracts/generated/typescript/TimelineNotice'
import type { TimelineEntry } from '../../../packages/contracts/live'
import { describe, expect, it } from 'vitest'
import { chatQuota } from '../src/chat-quota'
import {
  engineRetryDetail,
  engineRetryTitle,
  limitReset,
  usageLimitDetail,
  usageLimitTitle,
} from '../src/engine-status'
import { noticeArtifact, noticeShown } from '../src/notices'

const retry = {
  attempt: 2,
  maxRetries: 10,
  reason: 'overloaded',
  httpStatus: 529,
  delayMs: 4000,
  noResponse: false,
}

function limit(status: string, blocked = false, resetsAt: number | null = null) {
  return {
    status,
    window: 'five_hour',
    blocked,
    resetsAt,
  }
}

function card(position: number, notice: Partial<TimelineNotice>): TimelineEntry {
  return {
    id: `notice:${position}`,
    runId: 'run',
    position,
    revision: position,
    source: position,
    turn: 1,
    attempt: 0,
    createdAt: position,
    updatedAt: position,
    content: {
      kind: 'notice',
      notice: {
        code: 'runStatus',
        state: 'info',
        prominent: false,
        ...notice,
      },
    },
  }
}

function present(notice: Partial<TimelineNotice>) {
  const entry = card(1, notice)
  return noticeArtifact(entry, entry.content.kind === 'notice' ? entry.content.notice : {} as TimelineNotice)
}

describe('engine status', () => {
  it('describes the engine retries and usage limits without calling them failures', () => {
    expect(engineRetryTitle(retry)).toBe('Claude Code is retrying the request · attempt 2/10')
    expect(engineRetryDetail(retry)).toBe('Servers overloaded · HTTP 529 · next attempt after 4 s')
    expect(engineRetryDetail({
      ...retry,
      noResponse: true,
      httpStatus: null,
      delayMs: 60000,
    })).toBe('No response from the API · next attempt after 60 s')
    expect(usageLimitTitle(limit('allowed_warning'))).toBe('Approaching the 5-hour limit')
    expect(usageLimitTitle({ ...limit('rejected', true), window: 'seven_day' })).toBe('Weekly limit reached')
    expect(usageLimitTitle({ ...limit('rejected'), window: null })).toBe('Usage limit reached · continuing on extra usage')
    expect(usageLimitTitle(limit('allowed'))).toBe('Usage limit no longer applies')
    expect(usageLimitDetail(limit('allowed'))).toBe('')
    const reset = new Date(2030, 0, 1, 12, 0).getTime()
    expect(limitReset(reset, reset)).toBe(new Date(reset).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }))
    expect(limitReset(reset, reset - 86_400_000)).toContain(new Date(reset).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit' }))
    expect(limitReset(reset, reset - 86_400_000).length).toBeGreaterThan(limitReset(reset, reset).length)
  })

  it('presents the server’s engine cards as notices, never as errors', () => {
    const latest = present({ code: 'engineRetry', prominent: true, engineRetry: retry })
    expect(latest).toMatchObject({ title: 'Claude Code is retrying the request · attempt 2/10', status: 'info', attention: true })
    expect(present({ code: 'engineRetry', recovered: true, engineRetry: retry })).toMatchObject({ statusLabel: 'Recovered', attention: false })
    expect(present({
      code: 'engineNotice',
      prominent: true,
      level: 'warning',
      detail: 'Context is almost full.',
    })).toMatchObject({
      title: 'Claude Code warning',
      blocks: [{ label: 'Details', code: 'Context is almost full.' }],
    })
  })

  it('keeps the recorded engine output available in the run log', () => {
    const entry = card(1, { code: 'engineRetry', engineRetry: retry })
    if (entry.content.kind !== 'notice')
      throw new Error('Expected notice')
    const source = {
      id: 1,
      runId: 'run',
      createdAt: 1,
      type: 'engine.retry',
      text: '{"retryAt":123}',
      payload: null,
    }
    expect(noticeArtifact(entry, entry.content.notice, source).blocks).toEqual([
      { label: 'Output', code: source.text, language: 'plaintext' },
    ])
  })

  it('stops showing a usage limit once its window resets', () => {
    const now = Date.now()
    const applies = {
      code: 'usageLimit' as const,
      state: 'info' as const,
      prominent: true,
      usageLimit: limit('rejected', true, now + 60_000),
    }
    expect(noticeShown(applies, now)).toBe(true)
    expect(noticeShown(applies, now + 120_000)).toBe(false)
    expect(noticeShown({ ...applies, prominent: false }, now)).toBe(false)
  })

  it('tells the conversation how Codex handles the selected speed', () => {
    expect(present({ code: 'speed', prominent: true, speed: 'standard' }).title).toBe('This turn runs at Standard speed')
    expect(present({ code: 'speed', prominent: true, speed: 'unconfirmed' }).title).toBe('Speed not confirmed for this turn')
    expect(present({ code: 'speed', prominent: true, speed: 'confirmed' }).title).toBe('Selected speed requested for this turn')
  })

  it('gives the usage banner the reset the engine reported on the current failure', () => {
    const resetsAt = new Date(2030, 0, 1, 12, 0).getTime()
    const message = 'Your usage allowance is used up for now.'
    const run = {
      id: 'run',
      status: 'running',
      accountWaitReason: 'Usage exhausted. Waiting for an available Claude Code account to resume.',
      snapshot: { agent: { provider: 'claude' } },
    } as Run
    const current = card(3, { code: 'failure', state: 'error', quota: { message, resetsAt, current: true } })
    expect(chatQuota(run, [current])).toMatchObject({ message, reset: limitReset(resetsAt) })
    // A limit from a turn the conversation moved past lends nothing to a later wait.
    const past = card(3, { code: 'failure', state: 'error', quota: { message, resetsAt, current: false } })
    expect(chatQuota(run, [past])).toMatchObject({ message: '', reset: null })
  })
})
