// Injected into a fixture document only; these are the shipped save and refresh functions.
import { nextTick } from 'vue'
import {
  api,
  installationTransport,
  refresh,
  state,
} from '../../apps/web/src/api'
import { useResourceSave } from '../../apps/web/src/resourceSave'

const observations: { route: string, method: string, resource: string }[] = []
window.addEventListener('cairn-transport-observation', (event) => {
  const { route, method, path } = (event as CustomEvent).detail
  observations.push({ route, method, resource: path.split('/')[1] })
})

Object.assign(window, {
  refreshMeasurement: {
    configure(installationId: string, csrf: string) {
      Object.assign(state, {
        installationId,
        csrf,
        authenticated: true,
        ready: true,
      })
    },
    route: () => state.transportRoute,
    stop: () => installationTransport.stop(),
    async sample(kind: string, id: string, value: { name: string }, strategy: string) {
      const saving = useResourceSave(id)
      observations.length = 0
      const started = performance.now()
      await saving.save(`/${kind}`, { body: () => value })
      const written = performance.now()
      if (strategy === 'global') {
        await refresh()
      }
      else {
        // A task name edit has no effect on other lists. This is an experimental
        // lower-cost read set, not a general invalidation implementation.
        state.tasks = await api('/tasks')
      }

      await nextTick()
      const list = kind === 'agents' ? state.agents : kind === 'projects' ? state.projects : state.tasks
      const published = list.some(item => item.id === id && item.name === value.name)
      return {
        published,
        writeMs: written - started,
        totalMs: performance.now() - started,
        requests: [...observations],
      }
    },
  },
})
