import type { RunListItem } from '../../../packages/contracts/contracts'
import type { OutcomeAsideAction } from '../../../packages/contracts/generated/typescript/OutcomeAsideAction'
import { setOutcomeAside } from '../../../packages/contracts/generated/setOutcomeAside'
import { api, notify } from './api'
import { patchMissionRun, refreshMissionRuns } from './mission-runs'

type Outcome = Pick<RunListItem, 'id' | 'taskId' | 'finishedAt'>

// Shown at once; the refresh that follows confirms it, or restores the server's state on failure.
async function send(run: Outcome, action: OutcomeAsideAction, dismissal: RunListItem['dismissal'], message: string) {
  const rollback = patchMissionRun(run.id, { dismissal })
  try {
    await setOutcomeAside(api, { taskId: run.taskId, action })
    notify(message)
  }
  catch (error) {
    rollback()
    notify((error as Error).message)
  }

  await refreshMissionRuns()
}

/**
 * Sets a blocked mission's outcome aside for the signed-in account, on every device: dismissed,
 * or snoozed `until` a time. It asks again when the mission finishes another run.
 */
export function setAside(run: Outcome, until: number | null = null) {
  const message = until ? `Snoozed until ${new Date(until).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })}` : 'Dismissed until the mission reports again'
  const finishedAt = run.finishedAt ?? 0
  const action: OutcomeAsideAction = until ? { kind: 'snooze', finishedAt, until } : { kind: 'dismiss', finishedAt }
  return send(run, action, { until }, message)
}

/** Brings a dismissed or snoozed outcome back to For you. */
export function restoreOutcome(run: Outcome) {
  return send(run, { kind: 'restore' }, null, 'Back in For you')
}
