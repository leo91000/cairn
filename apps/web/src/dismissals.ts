import type { RunListItem } from '../../../packages/contracts/contracts'
import { api, notify } from './api'
import { patchMissionRun, refreshMissionRuns } from './mission-runs'

type Outcome = Pick<RunListItem, 'id' | 'taskId' | 'finishedAt'>

// Shown at once; the refresh that follows confirms it, or restores the server's state on failure.
async function send(run: Outcome, item: Record<string, unknown>, dismissal: RunListItem['dismissal'], message: string) {
  patchMissionRun(run.id, { dismissal })
  try {
    await api('/dismissals', { method: 'POST', body: JSON.stringify({ items: [{ key: `task:${run.taskId}`, ...item }] }) })
    notify(message)
  }
  catch (error) {
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
  return send(run, { finishedAt: run.finishedAt, until }, { until }, message)
}

/** Brings a dismissed or snoozed outcome back to For you. */
export function restoreOutcome(run: Outcome) {
  return send(run, { restore: true }, null, 'Back in For you')
}
