import type { Provider } from '../../../packages/contracts/accounts'
import type { ModelCatalog } from '../../../packages/contracts/models'
import { reactive } from 'vue'
import { providers } from '../../../packages/contracts/accounts'
import { listModels } from '../../../packages/contracts/generated/listModels'
import { api, state } from './api'

// The last catalog is kept locally so pickers can label saved models without
// fetching on every page load; opening a picker refreshes it.
const installationScope = state.installationId ? `${state.installationId}:` : ''

function stored(key: string): Pick<ModelCatalog, 'models' | 'connected'> {
  try {
    const value = JSON.parse(localStorage.getItem(`${installationScope}${key}`) || 'null')
    return { models: value?.models ?? [], connected: typeof value?.connected === 'boolean' ? value.connected : undefined }
  }
  catch {
    return { models: [] }
  }
}

function store(key: string, catalog: ModelCatalog) {
  try {
    localStorage.setItem(`${installationScope}${key}`, JSON.stringify({ models: catalog.models, connected: catalog.connected }))
  }
  catch {}
}

function createCatalog(provider: Provider) {
  const key = `cairn-models:${provider}`
  const saved = stored(key)
  const catalog = reactive({
    models: saved.models,
    // Unknown until a listing says so, or from an installation that predates it.
    connected: saved.connected as boolean | undefined,
    checkedAt: null as number | null,
    stale: false,
    error: '',
    loading: false,
    loaded: false,
    nativeDefaults: undefined as ModelCatalog['nativeDefaults'],
  })
  let pending: Promise<void> | undefined
  let revision = 0

  function load() {
    if (pending)
      return pending
    const requestRevision = revision
    catalog.loading = true
    pending = listModels(api, { provider })
      .then((value) => {
        if (requestRevision !== revision)
          return
        Object.assign(catalog, value, { nativeDefaults: value.nativeDefaults, connected: value.connected })
        store(key, value)
      })
      .catch((error: Error) => {
        if (requestRevision === revision)
          catalog.error = error.message
      })
      .finally(() => {
        if (requestRevision === revision) {
          catalog.loading = false
          catalog.loaded = true
        }

        pending = undefined
      })
      .then(() => {})
    return pending
  }

  async function refresh() {
    // A request admitted before a configuration save may contain the old defaults.
    ++revision
    catalog.loaded = false
    await pending
    await load()
  }

  return { catalog, load, refresh }
}

export const modelCatalogs = Object.fromEntries(
  (Object.keys(providers) as Provider[]).map(provider => [provider, createCatalog(provider)]),
) as Record<Provider, ReturnType<typeof createCatalog>>
