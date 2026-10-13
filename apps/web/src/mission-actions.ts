import type { RunListItem, Task } from '../../../packages/contracts/contracts'
import { ref } from 'vue'
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

  async function save(task: Task, message: string, method = 'PUT', path = `/tasks/${task.id}`) {
    try {
      await api(path, { method, body: JSON.stringify(task) })
      await refresh()
      notify(message)
    }
    catch (e) { error.value = (e as Error).message }
  }

  const pause = (task: Task) => save({ ...task, enabled: !task.enabled }, task.enabled ? 'Schedule paused' : 'Schedule resumed')
  const archive = (task: Task) => save({ ...task, archived: !task.archived, enabled: false }, task.archived ? 'Mission restored with its schedule paused' : 'Mission archived')

  function duplicate(task: Task) {
    return save({
      ...task,
      name: `${task.name.slice(0, 92)} (copy)`,
      enabled: false,
      archived: false,
    }, 'Mission duplicated with its schedule paused', 'POST', '/tasks')
  }

  async function remove() {
    const task = deleting.value!
    try {
      await api(`/tasks/${task.id}`, { method: 'DELETE' })
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
