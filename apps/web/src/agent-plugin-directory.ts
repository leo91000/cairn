import type { PluginCards, PluginInstanceAccess } from './plugin-catalogue'
import type {
  Catalog,
  Manifest,
  PluginConnection,
  PluginEntry,
} from './plugins'
import { api, ApiError } from './api'
import { pluginManagementItems } from './plugin-management'

/** Saved access metadata; this directory never authorizes execution. */
export interface AgentPluginPackage {
  id: string
  status: PluginEntry['status']
  active: string | null
  name: string
  instances?: Manifest['instances']
  agentAccess: boolean
  agentAccessDescription: string
  operations: number
}
export type AgentPluginInstance = Pick<PluginConnection, 'id' | 'pluginId' | 'name' | 'enabled' | 'agentId' | 'agentIds' | 'status'>
export interface AgentPluginDirectory {
  packages: AgentPluginPackage[]
  connections: AgentPluginInstance[]
  resourceGrantCounts?: Record<string, number>
  /** Only the explicit older-host adapter retains full manifests for chosen schemas. */
  legacyPackages?: PluginEntry[]
}

export function agentPluginPackage(entry: PluginEntry): AgentPluginPackage {
  return {
    id: entry.id,
    status: entry.status,
    active: entry.active,
    name: entry.manifest?.name ?? entry.id,
    instances: entry.manifest?.instances,
    agentAccess: Boolean(entry.manifest?.agentAccess),
    agentAccessDescription: entry.manifest?.agentAccess?.description ?? '',
    operations: entry.manifest?.operations?.length ?? 0,
  }
}

async function legacy(signal: AbortSignal): Promise<AgentPluginDirectory> {
  const catalog = await api<Catalog>('/plugins', { signal })
  if (!Array.isArray(catalog.packages) || !Array.isArray(catalog.connections))
    throw new Error('Saved plugin access could not be loaded.')
  return {
    packages: catalog.packages.map(agentPluginPackage),
    connections: catalog.connections,
    resourceGrantCounts: catalog.resourceGrantCounts,
    legacyPackages: catalog.packages,
  }
}

/** Complete every page. A supported read failing must never become a partial directory. */
export async function agentPluginDirectory(signal: AbortSignal): Promise<AgentPluginDirectory> {
  let first: PluginCards
  try {
    first = await api<PluginCards>('/plugins/packages', { signal })
  }
  catch (error) {
    if (!(error instanceof ApiError) || error.status !== 404)
      throw error
    return legacy(signal)
  }

  if (first.packageReads !== true || !Array.isArray(first.items))
    throw new Error('The plugin directory response is incomplete.')
  if (first.agentReads !== true)
    return legacy(signal)
  const cards = await pluginManagementItems('/plugins/packages', signal, first)
  const connections = await pluginManagementItems<PluginInstanceAccess & AgentPluginInstance>('/plugins/instance-access', signal)
  const counts = await pluginManagementItems<{ id: string, count: number }>('/plugins/agent-grant-counts', signal)
  if (connections.some(item => !item.id || !item.pluginId || typeof item.agentId !== 'string'
    || typeof item.name !== 'string' || typeof item.enabled !== 'boolean'
    || !Array.isArray(item.agentIds) || item.agentIds.some(id => typeof id !== 'string')
    || typeof item.status?.state !== 'string')
  || counts.some(item => !item.id || !Number.isSafeInteger(item.count) || item.count < 0)
  || cards.some(item => item.presentation && (typeof item.presentation.agentAccess !== 'boolean'
    || !Number.isSafeInteger(item.presentation.contributions?.operations)
    || item.presentation.contributions.operations! < 0))) {
    throw new Error('Saved plugin access could not be loaded completely.')
  }

  return {
    packages: cards.map(entry => ({
      id: entry.id,
      status: entry.status,
      active: entry.active,
      name: entry.presentation?.name ?? entry.id,
      instances: entry.presentation?.instances,
      agentAccess: entry.presentation?.agentAccess ?? false,
      agentAccessDescription: entry.presentation?.agentAccessDescription ?? '',
      operations: entry.presentation?.contributions.operations ?? 0,
    })),
    connections,
    resourceGrantCounts: Object.fromEntries(counts.map(item => [item.id, item.count])),
  }
}

/** Permission schemas belong to the chosen package, never to a card projection. */
export async function agentPluginManifest(directory: AgentPluginDirectory, id: string, signal: AbortSignal): Promise<Manifest> {
  const entry = directory.legacyPackages
    ? directory.legacyPackages.find(entry => entry.id === id)
    : await api<Omit<PluginEntry, 'versions'>>(`/plugins/packages/${encodeURIComponent(id)}`, { signal })
  if (entry?.id !== id || !entry.active || !entry.manifest || entry.manifest.id !== id)
    throw new Error('The selected plugin schema is unavailable. Refresh before changing access.')
  return entry.manifest
}
