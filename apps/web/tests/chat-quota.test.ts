import type { Run, RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import { chatQuota, quotaEntries } from '../src/chat-quota'

const message = 'You\'ve hit your individual spend limit · run /usage-credits to ask your admin for a higher limit · your weekly limit resets 3pm (UTC)'
const run = {
  id: 'run',
  status: 'running',
  accountWaitReason: 'Usage exhausted. Waiting for an available Claude Code account to resume.',
  snapshot: { agent: { provider: 'claude' } },
} as Run

function event(id: number, type: string, payload: Record<string, unknown> = {}, text = ''): RunEvent {
  return {
    id,
    runId: 'run',
    createdAt: id * 1000,
    type,
    text,
    payload,
  }
}

const events = [
  event(1, 'turn.started'),
  event(2, 'item.completed', { item: { id: 'work', type: 'agent_message', text: 'Work is preserved.' } }),
  event(3, 'item.completed', { item: { id: 'quota', type: 'agent_message', text: message } }),
  event(4, 'turn.failed', { error: { message } }),
  event(5, 'status', {}, 'failed'),
]

describe('conversation quota presentation', () => {
  it('folds the duplicate assistant message, turn failure and terminal status, preserving work', () => {
    const quota = chatQuota(run, events)!
    expect(quota).toMatchObject({
      provider: 'Claude Code',
      spend: true,
      automatic: true,
      reset: '3pm (UTC)',
    })
    const entries = quotaEntries(activityEntries(events, true), events)
    expect(entries).toHaveLength(1)
    expect(entries[0]).toMatchObject({ kind: 'message', text: 'Work is preserved.' })
    expect(activityEntries(events, false).length).toBeGreaterThan(entries.length)
  })

  it('requires manual resume for saved failures and paused conversations', () => {
    expect(chatQuota({ ...run, status: 'failed', error: message }, events)?.automatic).toBe(false)
    expect(chatQuota(run, events, true)?.automatic).toBe(false)
    expect(chatQuota({ ...run, status: 'succeeded', accountWaitReason: null }, events)).toBeNull()
    expect(chatQuota({ ...run, status: 'running', accountWaitReason: null }, events)).toBeNull()
  })

  it('does not hide tool output, user quotations, earlier turns or unrelated errors', () => {
    const history = [
      event(-1, 'item.completed', { item: { id: 'old', type: 'agent_message', text: message } }),
      event(0, 'chat.user', { text: message }),
      ...events,
      event(6, 'item.completed', {
        item: {
          id: 'tool',
          type: 'command_execution',
          command: 'cat log',
          aggregated_output: message,
          exit_code: 1,
        },
      }),
      event(7, 'error', { message: 'An unrelated error' }),
    ]
    const result = quotaEntries(activityEntries(history, true), history)
    expect(result.some(entry => entry.kind === 'message' && entry.text === message && entry.role !== 'user')).toBe(true)
    expect(result.some(entry => entry.kind === 'message' && entry.role === 'user')).toBe(true)
    expect(result.some(entry => entry.kind === 'group')).toBe(true)
    expect(result.some(entry => entry.kind === 'notice' && entry.artifact.blocks.some(block => block.code === 'An unrelated error'))).toBe(true)
    expect(chatQuota({ ...run, status: 'failed', error: 'Authentication failed' }, history)).toBeNull()
  })

  it('shows an admission wait even when the failure is outside the loaded page, without inventing a reset', () => {
    const quota = chatQuota({ ...run, accountWaitReason: 'Waiting for a Claude Code account with available usage.' }, [])!
    expect(quota).toMatchObject({ automatic: true, message: '', reset: null })
    expect(chatQuota({ ...run, accountWaitReason: 'Waiting for a free Claude Code account slot.' }, events)).toBeNull()
  })

  it('keeps recovered and repeated quota interruptions folded, including legacy JSON text events', () => {
    const legacy = events.map(event => event.type === 'turn.failed'
      ? { ...event, payload: undefined, text: JSON.stringify({ type: event.type, ...event.payload }) }
      : event)
    const resumed = [
      ...legacy,
      event(6, 'turn.started'),
      event(7, 'item.completed', { item: { id: 'quota', type: 'agent_message', text: message } }),
      event(8, 'turn.failed', { error: { message } }),
    ]
    expect(quotaEntries(activityEntries(resumed, true), resumed)).toHaveLength(1)
    const recovered = { ...run, status: 'succeeded' as const, accountWaitReason: null }
    expect(chatQuota(recovered, resumed)).toBeNull()
    expect(quotaEntries(activityEntries(resumed, true), resumed)).toHaveLength(1)
    expect(activityEntries(resumed, false).length).toBeGreaterThan(1)
  })

  it('folds plain historical replies but preserves legacy command results', () => {
    const history = [
      event(1, 'turn.started'),
      { ...event(2, 'item.started', {}, 'cat log'), payload: undefined },
      { ...event(3, 'item.completed', {}, message), payload: undefined },
      { ...event(4, 'item.completed', {}, message), payload: undefined },
      { ...event(5, 'turn.failed', {}, JSON.stringify({ type: 'turn.failed', error: { message } })), payload: undefined },
    ]
    const entries = quotaEntries(activityEntries(history, true), history)
    expect(entries).toHaveLength(1)
    expect(entries[0]).toMatchObject({ kind: 'group' })
  })

  it('uses structured quota codes and respects native willRetry notifications', () => {
    const text = 'Account usage is exhausted.'
    for (const code of [{ code: 'usage_limit_reached' }, { codexErrorInfo: 'usageLimitExceeded' }]) {
      const history = [
        event(1, 'turn.started'),
        event(2, 'item.completed', { item: { id: 'echo', type: 'agent_message', text } }),
        event(3, 'error', { ...code, message: text }),
      ]
      expect(chatQuota(run, history)?.message).toBe(text)
      expect(quotaEntries(activityEntries(history, true), history)).toHaveLength(0)
      const retrying = [...history.slice(0, 2), event(3, 'error', { ...code, message: text, willRetry: true })]
      expect(quotaEntries(activityEntries(retrying, true), retrying)).toHaveLength(2)
    }
  })
})
