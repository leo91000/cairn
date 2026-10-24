import type { RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import { compactionText, contextSummary } from '../src/context-usage'

function event(id: number, type: string, payload: Record<string, unknown> = {}): RunEvent {
  return {
    id,
    runId: 'run',
    createdAt: id * 1000,
    type,
    text: '',
    payload,
  }
}

describe('context usage', () => {
  it('shows the latest request against the reported window', () => {
    const summary = contextSummary({
      contextTokens: 42_202,
      contextWindow: 200_000,
      cachedTokens: 40_002,
      estimated: false,
    })
    expect(summary.percent).toBe(21)
    expect(summary.short).toBe('42K / 200K · 21%')
    expect(summary.label).toBe('42,202 of 200,000 tokens (21%)')
    expect(summary.details).toEqual([
      'Measured from the latest model request',
      'Window 200,000 tokens',
      '40,002 tokens read from cache',
    ])
  })

  it('keeps an unknown window and missing reports unavailable', () => {
    const unknown = contextSummary({
      contextTokens: 12_000,
      contextWindow: null,
      cachedTokens: null,
      estimated: false,
    })
    expect(unknown.percent).toBeNull()
    expect(unknown.short).toBe('12K tokens')
    expect(unknown.label).toContain('window unknown')
    expect(unknown.details).toContain('Window size not reported')

    for (const usage of [null, undefined, {
      contextTokens: 0,
      contextWindow: 200_000,
      cachedTokens: null,
      estimated: false,
    }]) {
      const missing = contextSummary(usage)
      expect(missing.percent).toBeNull()
      expect(missing.label).toBe('Context usage unavailable')
    }
  })

  it('marks estimates', () => {
    const estimate = contextSummary({
      contextTokens: 6_200,
      contextWindow: 1_000_000,
      cachedTokens: null,
      estimated: true,
    })
    expect(estimate.short).toBe('≈ 6K / 1M · 1%')
    expect(estimate.details[0]).toBe('Estimated by the coding agent')
  })
})

describe('context compaction', () => {
  it('shows one divider per compaction without hiding the earlier history', () => {
    const entries = activityEntries([
      event(1, 'chat.user', { text: 'First request' }),
      event(2, 'item.completed', { item: { id: 'reply', type: 'agent_message', text: 'Done.' } }),
      event(3, 'context.usage', {
        usage: {
          contextTokens: 150_000,
          contextWindow: 200_000,
          cachedTokens: null,
          estimated: false,
        },
      }),
      event(4, 'turn.started'),
      event(5, 'item.started', { item: { id: 'compact-1', type: 'context_compaction', status: 'in_progress' } }),
      event(6, 'item.completed', {
        item: {
          id: 'compact-1',
          type: 'context_compaction',
          status: 'completed',
          trigger: 'manual',
          pre_tokens: 150_000,
          post_tokens: 9_100,
        },
      }),
      event(7, 'turn.completed'),
    ], true)

    expect(entries.map(entry => entry.kind)).toEqual(['message', 'message', 'compaction'])
    const compaction = entries[2]!
    expect(compaction).toMatchObject({
      kind: 'compaction',
      state: 'done',
      trigger: 'manual',
      preTokens: 150_000,
      postTokens: 9_100,
    })
    if (compaction.kind === 'compaction')
      expect(compactionText(compaction)).toEqual({ title: 'Context compacted', detail: '150K → 9K tokens · Earlier messages stay visible here' })
  })

  it('shows running and failed compactions honestly', () => {
    const running = activityEntries([event(1, 'item.started', { item: { id: 'c', type: 'context_compaction', status: 'in_progress' } })], true)
    expect(running).toMatchObject([{ kind: 'compaction', state: 'running' }])
    const failed = activityEntries([
      event(1, 'item.started', { item: { id: 'c', type: 'context_compaction', status: 'in_progress' } }),
      event(2, 'item.completed', { item: { id: 'c', type: 'context_compaction', status: 'failed' } }),
    ], true)
    expect(failed).toMatchObject([{ kind: 'compaction', state: 'failed' }])
    if (failed[0]?.kind === 'compaction')
      expect(compactionText(failed[0]).title).toBe('Context compaction failed')
  })
})
