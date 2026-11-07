import type { Run } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import { describe, expect, it } from 'vitest'
import { chatQuota } from '../src/chat-quota'

const message = 'You\'ve hit your individual spend limit · run /usage-credits to ask your admin for a higher limit · your weekly limit resets 3pm (UTC)'
const run = {
  id: 'run',
  status: 'running',
  accountWaitReason: 'Usage exhausted. Waiting for an available Claude Code account to resume.',
  snapshot: { agent: { provider: 'claude' } },
} as Run

/** The server's usage-limit failure card; `current` until the turn or execution moves on. */
function failure(position: number, text: string, current = true): TimelineEntry {
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
        code: 'failure',
        state: 'error',
        prominent: false,
        origin: 'turn',
        detail: text,
        quota: { message: text, resetsAt: null, current },
      },
    },
  }
}

const cards = [failure(4, message)]

describe('conversation quota presentation', () => {
  it('reads the current usage-limit failure for an automatic wait', () => {
    expect(chatQuota(run, cards)).toMatchObject({
      provider: 'Claude Code',
      message,
      spend: true,
      automatic: true,
      reset: '3pm (UTC)',
    })
  })

  it('requires manual resume for saved failures', () => {
    expect(chatQuota({ ...run, status: 'failed', error: message }, cards)?.automatic).toBe(false)
    expect(chatQuota({ ...run, status: 'succeeded', accountWaitReason: null }, cards)).toBeNull()
    expect(chatQuota({ ...run, status: 'running', accountWaitReason: null }, cards)).toBeNull()
    expect(chatQuota({ ...run, status: 'failed', error: 'Authentication failed' }, cards)).toBeNull()
  })

  it('shows an admission wait even when the failure is outside the loaded page, without inventing a reset', () => {
    const quota = chatQuota({ ...run, accountWaitReason: 'Waiting for a Claude Code account with available usage.' }, [])!
    expect(quota).toMatchObject({ automatic: true, message: '', reset: null })
    expect(chatQuota({ ...run, accountWaitReason: 'Waiting for a free Claude Code account slot.' }, cards)).toBeNull()
  })

  it('does not borrow recovered quota details for later admission or provider changes', () => {
    const recovered = [failure(4, message, false)]
    const admission = {
      ...run,
      status: 'queued' as const,
      accountWaitReason: 'Waiting for a Codex account with available usage.',
      snapshot: { ...run.snapshot, agent: { ...run.snapshot.agent, provider: 'codex' as const } },
    }
    expect(chatQuota(admission, recovered)).toMatchObject({
      provider: 'Codex',
      message: '',
      spend: false,
      reset: null,
      automatic: true,
    })
    expect(chatQuota(admission, cards)?.message).toBe('')
    expect(chatQuota(run, recovered)?.message).toBe('')

    const currentMessage = 'You\'ve hit your usage limit · resets Tuesday 9am (UTC)'
    const interrupted = [...recovered, failure(10, currentMessage)]
    expect(chatQuota(run, interrupted)).toMatchObject({ message: currentMessage, spend: false, reset: 'Tuesday 9am (UTC)' })
    expect(chatQuota({ ...run, status: 'failed', error: currentMessage }, cards)?.message).toBe(currentMessage)
  })
})
