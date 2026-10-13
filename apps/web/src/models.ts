import type { Provider } from '../../../packages/contracts/accounts'
import type { ModelCatalog } from '../../../packages/contracts/models'
import { reactive } from 'vue'
import { providers } from '../../../packages/contracts/accounts'
import { api, state } from './api'

// The last catalog is kept locally so pickers can label saved models without
// fetching on every page load; opening a picker refreshes it.
const installationScope = state.installationId ? `${state.installationId}:` : ''

function stored(key: string): ModelCatalog['models'] {
  try {
    return JSON.parse(localStorage.getItem(`${installationScope}${key}`) || 'null')?.models ?? []
  }
  catch {
    return []
  }
}

function store(key: string, catalog: ModelCatalog) {
  try {
    localStorage.setItem(`${installationScope}${key}`, JSON.stringify({ models: catalog.models }))
  }
  catch {}
}

function createCatalog(provider: Provider) {
  const key = `cairn-models:${provider}`
  const catalog = reactive({
    models: stored(key),
    checkedAt: null as number | null,
    stale: false,
    error: '',
    loading: false,
  })
  let pending: Promise<void> | undefined

  function load() {
    if (pending)
      return pending
    catalog.loading = true
    pending = api<ModelCatalog>(`/${provider}/models`)
      .then((value) => {
        Object.assign(catalog, value)
        store(key, value)
      })
      .catch((error: Error) => { catalog.error = error.message })
      .finally(() => {
        catalog.loading = false
        pending = undefined
      })
      .then(() => {})
    return pending
  }

  return { catalog, load }
}

export const modelCatalogs = Object.fromEntries(
  (Object.keys(providers) as Provider[]).map(provider => [provider, createCatalog(provider)]),
) as Record<Provider, ReturnType<typeof createCatalog>>
