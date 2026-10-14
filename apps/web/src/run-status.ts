import type { Run } from '../../../packages/contracts/contracts'

type RunState = Pick<Run, 'suspendedByExclusive'> & { status: Run['status'] | 'idle' }

export const exclusiveSuspensionLabel = 'Suspended for exclusive work'

/** A late reservation snapshot must never hide a settled or queued execution. */
export function isExclusivelySuspended(run?: RunState | null) {
  return run?.status === 'running' && !!run.suspendedByExclusive
}

/** Presentation only: the run remains active for cancellation and queued input. */
export function runDisplayStatus(run: RunState) {
  return isExclusivelySuspended(run) ? 'suspended' : run.status
}
