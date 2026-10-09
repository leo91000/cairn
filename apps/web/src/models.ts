import type { ModelCatalog } from '../../../packages/contracts/models'
import { reactive } from 'vue'
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

export const modelCatalog = reactive({
  models: stored('cairn-models:codex'),
  checkedAt: null as number | null,
  stale: false,
  error: '',
  loading: false,
})
let pending: Promise<void> | undefined
export function loadModels() {
  if (pending)
    return pending
  modelCatalog.loading = true
  pending = api<ModelCatalog>('/codex/models')
    .then((catalog) => {
      Object.assign(modelCatalog, catalog)
      store('cairn-models:codex', catalog)
    })
    .catch((error: Error) => { modelCatalog.error = error.message })
    .finally(() => {
      modelCatalog.loading = false
      pending = undefined
    })
    .then(() => {})
  return pending
}

export const claudeCatalog = reactive({
  models: stored('cairn-models:claude'),
  checkedAt: null as number | null,
  stale: false,
  error: '',
  loading: false,
})
let claudePending: Promise<void> | undefined
export function loadClaudeModels() {
  if (claudePending)
    return claudePending
  claudeCatalog.loading = true
  claudePending = api<ModelCatalog>('/claude/models')
    .then((catalog) => {
      Object.assign(claudeCatalog, catalog)
      store('cairn-models:claude', catalog)
    })
    .catch((error: Error) => { claudeCatalog.error = error.message })
    .finally(() => {
      claudeCatalog.loading = false
      claudePending = undefined
    })
    .then(() => {})
  return claudePending
}
