import type {
  Catalog,
  Manifest,
  PackageVersion,
  PluginConnection,
  PluginEntry,
  PluginPage,
} from './plugins'
import { api, ApiError } from './api'
import { pluginManagementItems } from './plugin-management'
import { pluginSetup } from './plugin-setup'
import { delegates, needsWorker } from './plugins'

/** Card data is presentation, never a manifest used to configure or approve code. */
export interface PluginPresentation {
  name: string
  description: string
  version: string
  brand?: Manifest['brand']
  instances?: Manifest['instances']
  capabilities: string[]
  needsWorker: boolean
  delegates: boolean
  agentAccess: boolean
  agentAccessDescription?: string | null
  setup?: ReturnType<typeof pluginSetup>
  contributions: {
    commands: number
    tools: number
    instructions: boolean
    view: boolean
    test: boolean
    form: boolean
    collections: number
    operations?: number
  }
}

export interface PluginCardEntry {
  id: string
  builtin: boolean
  status: PluginEntry['status']
  active: string | null
  presentation: PluginPresentation | null
  versionCount: number
  summary: {
    instances: number
    states: Record<string, number>
    pendingRequests: number
    authorizedAgents: number | null
    observedAt?: number
  }
}

export interface PluginCards extends PluginPage<PluginCardEntry> {
  packageReads: boolean
  instanceReads?: boolean
  accessReads?: boolean
  agentReads?: boolean
  setupDrafts?: boolean
  capabilities: Catalog['capabilities']
}

function legacyCards(catalog: Catalog): PluginCards {
  return {
    packageReads: false,
    setupDrafts: catalog.setupDrafts,
    capabilities: catalog.capabilities,
    items: catalog.packages.map((entry) => {
      const manifest = entry.manifest
      const instances = catalog.connections.filter(instance => instance.pluginId === entry.id)
      const states: Record<string, number> = {}
      for (const instance of instances) {
        const state = instance.enabled ? instance.status.state : 'paused'
        states[state] = (states[state] ?? 0) + 1
      }

      return {
        id: entry.id,
        builtin: entry.builtin,
        status: entry.status,
        active: entry.active,
        versionCount: entry.versions.length,
        presentation: manifest && {
          name: manifest.name,
          description: manifest.description,
          version: manifest.version,
          brand: manifest.brand,
          instances: manifest.instances,
          capabilities: manifest.capabilities,
          needsWorker: needsWorker(manifest),
          delegates: delegates(manifest),
          agentAccess: Boolean(manifest.agentAccess),
          setup: pluginSetup(entry),
          contributions: {
            commands: manifest.commands?.length ?? 0,
            tools: manifest.tools?.length ?? 0,
            instructions: Boolean(manifest.instructions),
            view: Boolean(manifest.instanceView),
            test: Boolean(manifest.instanceTest),
            form: Boolean(manifest.instanceForm),
            collections: manifest.resources?.length ?? 0,
          },
        },
        summary: {
          instances: instances.length,
          states,
          pendingRequests: instances.reduce((n, instance) => n + instance.requests.length, 0),
          // Older hosts only expose per-agent resource totals across all packages.
          authorizedAgents: Object.values(catalog.resourceGrantCounts ?? {}).some(count => count > 0)
            ? null
            : new Set(instances.flatMap(instance => instance.agentIds ?? [])).size,
        },
      }
    }),
  }
}

/** Older hosts advertise neither the instance summaries nor their exact read APIs. */
export async function pluginCards(signal: AbortSignal): Promise<PluginCards> {
  let first: PluginCards
  try {
    first = await api<PluginCards>('/plugins/packages', { signal })
  }
  catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404)
      throw error
    return legacyCards(await api<Catalog>('/plugins', { signal }))
  }

  if (first.packageReads !== true || !Array.isArray(first.items) || !Array.isArray(first.capabilities))
    throw new Error('The plugin catalogue response is incomplete. Refresh before making changes.')
  if (first.instanceReads !== true)
    return legacyCards(await api<Catalog>('/plugins', { signal }))
  const items = await pluginManagementItems<PluginCardEntry>('/plugins/packages', signal, first)
  const count = (value: unknown) => typeof value === 'number' && Number.isSafeInteger(value) && value >= 0
  if (items.some(({ summary }) => !summary || !summary.states
    || !count(summary.instances) || !count(summary.pendingRequests) || !count(summary.authorizedAgents)
    || Object.values(summary.states).some(value => !count(value)))) {
    throw new Error('Plugin summaries could not be loaded. Refresh the catalogue.')
  }

  return { ...first, items, nextCursor: null }
}

export async function pluginReview(id: string, signal: AbortSignal): Promise<{ entry: PluginEntry, lazy: boolean }> {
  const root = `/plugins/packages/${encodeURIComponent(id)}`
  let detail: Omit<PluginEntry, 'versions'>
  try {
    detail = await api<Omit<PluginEntry, 'versions'>>(root, { signal })
  }
  catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404)
      throw error
    const catalog = await api<Catalog>('/plugins', { signal })
    const entry = catalog.packages.find(entry => entry.id === id)
    if (!entry)
      throw new Error('Plugin package not found.')
    return { entry, lazy: false }
  }

  if (detail.id !== id)
    throw new Error('The plugin configuration response does not match this package.')
  const versions = await pluginManagementItems<PackageVersion>(`${root}/versions`, signal)
  if (versions.some(version => !version.version || (!version.error && (!version.digest || typeof version.approved !== 'boolean'))))
    throw new Error('The plugin version list is incomplete. Reopen the review.')
  return { entry: { ...detail, versions }, lazy: true }
}

/** Configuration owns one complete manifest; review history is a separate read. */
export type PluginConfigurationEntry = Omit<PluginEntry, 'versions'>

/** Saved IDs for eligibility and labels. These are not effective permissions. */
export interface PluginInstanceAccess {
  id: string
  pluginId: string
  pluginName: string
  name: string
  enabled: boolean
  agentIds: string[]
  resourceAgentIds: string[]
}

export interface PluginConfiguration {
  entry: PluginConfigurationEntry | null
  capabilities: Catalog['capabilities']
  setupDrafts: boolean
  exactInstanceReads: boolean
  connections: PluginConnection[]
  access: PluginInstanceAccess[]
  /** Older hosts only supply resource totals per agent across all packages. */
  resourceAgentIds: string[]
}

async function legacyConfiguration(id: string, signal: AbortSignal): Promise<PluginConfiguration> {
  const catalog = await api<Catalog>('/plugins', { signal })
  if (!Array.isArray(catalog.packages) || !Array.isArray(catalog.connections) || !Array.isArray(catalog.capabilities))
    throw new Error('The plugin configuration response is incomplete. Refresh before making changes.')
  const saved = catalog.packages.find(entry => entry.id === id)
  let entry: PluginConfigurationEntry | null = null
  if (saved) {
    const { versions: _versions, ...configuration } = saved
    entry = configuration
  }

  return {
    entry,
    capabilities: catalog.capabilities,
    setupDrafts: Boolean(catalog.setupDrafts),
    exactInstanceReads: false,
    connections: catalog.connections.filter(instance => instance.pluginId === id),
    access: catalog.connections.map(instance => ({
      id: instance.id,
      pluginId: instance.pluginId,
      pluginName: catalog.packages.find(entry => entry.id === instance.pluginId)?.manifest?.name ?? instance.pluginId,
      name: instance.name,
      enabled: instance.enabled,
      agentIds: instance.agentIds ?? [],
      resourceAgentIds: [],
    })),
    resourceAgentIds: Object.entries(catalog.resourceGrantCounts ?? {}).filter(([, count]) => count > 0).map(([id]) => id),
  }
}

/** A missing selected package is different from an unavailable configuration API. */
async function missingConfiguration(id: string, signal: AbortSignal): Promise<PluginConfiguration> {
  let support: PluginCards
  try {
    support = await api<PluginCards>('/plugins/packages', { signal })
  }
  catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404)
      throw error
    return legacyConfiguration(id, signal)
  }

  if (support.packageReads !== true || !Array.isArray(support.items) || !Array.isArray(support.capabilities))
    throw new Error('The plugin catalogue response is incomplete. Refresh before making changes.')
  if (support.instanceReads !== true || support.accessReads !== true)
    return legacyConfiguration(id, signal)
  return {
    entry: null,
    capabilities: support.capabilities,
    setupDrafts: Boolean(support.setupDrafts),
    exactInstanceReads: true,
    connections: [],
    access: [],
    resourceAgentIds: [],
  }
}

export async function pluginConfiguration(id: string, signal: AbortSignal): Promise<PluginConfiguration> {
  const root = `/plugins/packages/${encodeURIComponent(id)}`

  type Detail = PluginConfigurationEntry & {
    instanceReads?: boolean
    accessReads?: boolean
    setupDrafts?: boolean
    capabilities?: Catalog['capabilities']
  }

  let detail: Detail
  try {
    detail = await api<Detail>(root, { signal })
  }
  catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404)
      throw error
    return missingConfiguration(id, signal)
  }

  if (detail.id !== id)
    throw new Error('The plugin configuration response does not match this package.')
  if (detail.instanceReads !== true || detail.accessReads !== true)
    return legacyConfiguration(id, signal)
  if (!Array.isArray(detail.capabilities) || detail.manifest === undefined)
    throw new Error('The plugin configuration response is incomplete. Refresh before making changes.')
  const connections = await pluginManagementItems<PluginConnection>(`${root}/connections`, signal)
  const access = await pluginManagementItems<PluginInstanceAccess>('/plugins/instance-access', signal)
  if (connections.some(instance => instance.pluginId !== id)
    || access.some(instance => !instance.id || !instance.pluginId || !Array.isArray(instance.agentIds) || !Array.isArray(instance.resourceAgentIds))) {
    throw new Error('The saved plugin access response is incomplete. Refresh before making changes.')
  }

  return {
    entry: detail,
    capabilities: detail.capabilities,
    setupDrafts: Boolean(detail.setupDrafts),
    exactInstanceReads: true,
    connections,
    access,
    resourceAgentIds: [],
  }
}

/** Re-read all saved root IDs immediately before an older-host compatibility save. */
export async function pluginInstance(plugin: string, instance: string, exact: boolean) {
  if (!exact) {
    const catalog = await api<Catalog>('/plugins')
    const saved = catalog.connections.find(item => item.id === instance && item.pluginId === plugin)
    if (!saved)
      throw new Error('This plugin instance was removed. Reopen the plugin before saving.')
    return saved
  }

  const saved = await api<PluginConnection>(`/plugins/packages/${encodeURIComponent(plugin)}/connections/${encodeURIComponent(instance)}`)
  if (saved.id !== instance || saved.pluginId !== plugin || !Array.isArray(saved.agentIds))
    throw new Error('The saved plugin instance response is incomplete. Reopen the plugin before saving.')
  return saved
}
