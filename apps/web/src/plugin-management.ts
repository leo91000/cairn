import type { PluginPage } from './plugins'
import { api } from './api'

/** Owner management reads complete every page before exposing the saved list. */
export async function pluginManagementItems<T>(path: string, signal: AbortSignal, first?: PluginPage<T>): Promise<T[]> {
  const items: T[] = []
  const cursors = new Set<string>()
  let cursor: string | undefined | null
  do {
    signal.throwIfAborted()
    const target = cursor ? `${path}?cursor=${encodeURIComponent(cursor)}` : path
    const page = first ?? await api<PluginPage<T>>(target, { signal })
    first = undefined
    signal.throwIfAborted()
    items.push(...page.items)
    cursor = page.nextCursor
    if (cursor && cursors.has(cursor))
      throw new Error('The saved list changed while loading. Refresh it before making changes.')
    if (cursor)
      cursors.add(cursor)
  } while (cursor)

  return items
}
