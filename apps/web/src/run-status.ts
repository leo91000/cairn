import type { Run } from '../../../packages/contracts/contracts'

type RunState = Partial<Pick<Run, 'suspendedByExclusive'>> & { status: Run['status'] | 'idle' }

export const exclusiveSuspensionLabel = 'Suspended for exclusive work'

/** A late reservation snapshot must never hide a settled or queued execution. */
export function isExclusivelySuspended(run?: RunState | null) {
  return run?.status === 'running' && !!run.suspendedByExclusive
}

const quietStatuses = new Set(['Working', 'Ready'])

/**
 * The conversation header names a status only when it says more than the avatar: work in progress
 * shows on its ring, and a ready conversation needs nothing. A background task is named, since the
 * agent then waits rather than works.
 */
export function headerStatus(status: string) {
  if (quietStatuses.has(status))
    return null
  return { label: status, failed: status === 'Failed' || status === 'Interrupted' }
}

/** Presentation only: the run remains active for cancellation and queued input. */
export function runDisplayStatus(run: RunState) {
  return isExclusivelySuspended(run) ? 'suspended' : run.status
}
