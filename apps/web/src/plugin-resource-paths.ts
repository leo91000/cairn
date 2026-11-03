import type { PluginResourceRef } from '../../../packages/contracts/generated/typescript/PluginResourceRef'
import { resolvePluginResource } from '../../../packages/contracts/generated/resolvePluginResource'

const RESOURCE_PATH_CACHE_MS = 60_000
const resolvedResourcePaths = new Map<string, { path: string, expiresAt: number }>()
const pendingResourcePaths = new Map<string, Promise<string>>()

export function pluginResourcePathLabel(ancestors: readonly { name: string }[], resourceName: string) {
  return [...ancestors].reverse().map(parent => parent.name).concat(resourceName).join(' › ')
}

export function resolvePluginResourcePath(
  request: Parameters<typeof resolvePluginResource>[0],
  pluginId: string,
  instanceId: string,
  root: PluginResourceRef,
): Promise<string> {
  const cacheKey = `${pluginId}:${instanceId}:${root.resourceType}:${root.id}`
  const cached = resolvedResourcePaths.get(cacheKey)
  if (cached && cached.expiresAt > Date.now())
    return Promise.resolve(cached.path)
  const pending = pendingResourcePaths.get(cacheKey)
  if (pending)
    return pending

  const path = resolvePluginResource(request, {
    pluginId,
    instanceId,
    resourceType: root.resourceType,
    id: root.id,
  }).then((resolved) => {
    const label = pluginResourcePathLabel(resolved.ancestors, resolved.resource.name)
    resolvedResourcePaths.set(cacheKey, { path: label, expiresAt: Date.now() + RESOURCE_PATH_CACHE_MS })
    return label
  }).finally(() => {
    pendingResourcePaths.delete(cacheKey)
  })
  pendingResourcePaths.set(cacheKey, path)
  return path
}
