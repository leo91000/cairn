import type { Run } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { retryNotice } from '../src/run-retry'

const run = {
  status: 'queued',
  accountWaitReason: 'Temporary agent error. Retrying in 30 seconds (attempt 1/3).',
  retry: {
    attempt: 1,
    limit: 3,
    nextAttemptAt: 31_000,
    cause: { kind: 'timeout', message: 'workspace routing discovery timed out' },
  },
} as Run

describe('retry notice', () => {
  it('counts down and shows the next attempt without asking for user input', () => {
    expect(retryNotice(run, 1000)).toBe('Temporary agent error. Retrying in 30 seconds (attempt 1/3).')
    expect(retryNotice(run, 11_100)).toContain('in 20 seconds')
    expect(retryNotice(run, 31_000)).toContain('Retrying now')
  })

  it('keeps account and fencing waits visible and stops when the run starts', () => {
    expect(retryNotice({ ...run, accountRequired: 'codex' }, 1000)).toBeNull()
    expect(retryNotice({ ...run, accountWaitReason: 'Waiting for the previous VM to stop.' }, 1000)).toBeNull()
    expect(retryNotice({ ...run, status: 'running' }, 1000)).toBeNull()
    expect(retryNotice({ ...run, status: 'cancelled' }, 1000)).toBeNull()
  })
})
