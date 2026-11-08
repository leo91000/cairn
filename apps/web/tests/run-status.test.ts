import type { Run } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import {
  exclusiveSuspensionLabel,
  headerStatus,
  isExclusivelySuspended,
  runDisplayStatus,
} from '../src/run-status'

const reservation: NonNullable<Run['suspendedByExclusive']> = {
  id: 'reservation',
  nodeId: 'node',
  ownerRunId: 'owner',
  reason: 'Build the emulator image',
  state: 'acquired',
  renewalPending: false,
  error: null,
  createdAt: 1,
  expiresAt: 1000,
  waitUntil: 1000,
  pausedRunIds: ['peer'],
  minimumAvailableMemoryMiB: 0,
}

describe('exclusive suspension presentation', () => {
  it('shows a suspended active execution without changing its lifecycle status', () => {
    const run = { status: 'running' as const, suspendedByExclusive: reservation }
    expect(isExclusivelySuspended(run)).toBe(true)
    expect(runDisplayStatus(run)).toBe('suspended')
    expect(run.status).toBe('running')
    expect(runDisplayStatus({ status: 'running' })).toBe('running')
  })

  it.each(['succeeded', 'failed', 'cancelled', 'interrupted', 'queued', 'idle'] as const)(
    'keeps %s visible when an earlier reservation snapshot remains attached',
    (status) => {
      const run = { status, suspendedByExclusive: reservation }
      expect(isExclusivelySuspended(run)).toBe(false)
      expect(runDisplayStatus(run)).toBe(status)
    },
  )
})

describe('conversation header status', () => {
  it.each(['Working', 'Ready'])('stays quiet while %s', (status) => {
    expect(headerStatus(status)).toBeNull()
  })

  it.each(['Background task', 'Background tasks', 'Queued', 'Paused', 'Waiting for quota', exclusiveSuspensionLabel, 'Resumes in 7 min'])('names %s', (status) => {
    expect(headerStatus(status)).toEqual({ label: status, failed: false })
  })

  it.each(['Failed', 'Interrupted'])('marks %s as a failure', (status) => {
    expect(headerStatus(status)).toEqual({ label: status, failed: true })
  })
})
