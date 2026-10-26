import type { RunListItem, Task } from '../../../packages/contracts/contracts'
import type { MissionSettings } from '../../../packages/contracts/generated/typescript/MissionSettings'
import { ref } from 'vue'
import { createMission } from '../../../packages/contracts/generated/createMission'
import { deleteMission } from '../../../packages/contracts/generated/deleteMission'
import { setMissionState } from '../../../packages/contracts/generated/setMissionState'
import { api, notify, refresh } from './api'
import { refreshMissionRuns } from './mission-runs'

/**
 * What a mission's detail can do, shared by Missions and the Fil. `started` receives the queued
 * run so each place opens it where it belongs; `removed` lets the caller close what showed it.
 */
export function useMissionActions({ started, removed }: { started: (run: RunListItem) => unknown, removed?: (task: Task) => void }) {
  const busy = ref('')
  const error = ref('')
  const deleting = ref<Task | null>(null)

  async function run(task: Task) {
    busy.value = task.id
    error.value = ''
    try {
      const queued = await api<RunListItem>(`/tasks/${task.id}/run`, { method: 'POST' })
      notify('Mission added to the queue')
      void refreshMissionRuns()
      await started(queued)
    }
    catch (e) { error.value = (e as Error).message }
    finally { busy.value = '' }
  }

  async function state(task: Task, enabled: boolean, archived: boolean, message: string) {
    try {
      await setMissionState(api, {
        taskId: task.id,
        revision: task.revision,
        enabled,
        archived,
      })
      await refresh()
      notify(message)
    }
    catch (e) { error.value = (e as Error).message }
  }

  const pause = (task: Task) => state(task, !task.enabled, task.archived, task.enabled ? 'Schedule paused' : 'Schedule resumed')
  const archive = (task: Task) => state(task, false, !task.archived, task.archived ? 'Mission restored with its schedule paused' : 'Mission archived')

  async function duplicate(task: Task) {
    const settings: MissionSettings = {
      name: `${task.name.slice(0, 92)} (copy)`,
      prompt: task.prompt,
      agentId: task.agentId,
      projectId: task.projectId,
      skills: task.skills,
      tags: task.tags,
      cron: task.cron,
      timezone: task.timezone,
      enabled: false,
      archived: false,
      worktree: task.worktree,
    }
    try {
      await createMission(api, { settings })
      await refresh()
      notify('Mission duplicated with its schedule paused')
    }
    catch (e) { error.value = (e as Error).message }
  }

  async function remove() {
    const task = deleting.value!
    try {
      await deleteMission(api, { taskId: task.id })
      removed?.(task)
      deleting.value = null
      await refresh()
      notify('Mission removed')
    }
    catch (e) { error.value = (e as Error).message }
  }

  return {
    busy,
    error,
    deleting,
    run,
    pause,
    archive,
    duplicate,
    remove,
  }
}
