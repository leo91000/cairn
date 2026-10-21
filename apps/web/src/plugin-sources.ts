import type { PluginProject } from '../../../packages/contracts/contracts'
import type {
  Catalog,
  Manifest,
  PluginConnection,
  SettingField,
} from './plugins'
import { searchText } from './select'

/** A package's project or skill source, with the enabled instances that can import from it. */
export interface PluginSourceChoice {
  /** `pluginId:source`, unique across packages. */
  key: string
  pluginId: string
  source: string
  title: string
  description: string
  pluginName: string
  brand?: Manifest['brand']
  instances: PluginConnection[]
  /** The location's fields, for skill sources. */
  fields: SettingField[]
}

/**
 * The sources of a kind the catalogue offers: active packages that declare them and have at least
 * one enabled instance to import through.
 */
export function pluginSources(catalog: Pick<Catalog, 'packages' | 'connections'>, kind: 'projectSources' | 'skillSources'): PluginSourceChoice[] {
  return catalog.packages.flatMap((entry) => {
    const manifest = entry.manifest
    if (!entry.active || !manifest || !['active', 'update'].includes(entry.status))
      return []
    const instances = catalog.connections.filter(connection => connection.pluginId === entry.id && connection.enabled)
    if (!instances.length)
      return []

    const sources: {
      name: string
      title: string
      description?: string
      fields?: SettingField[]
    }[] = manifest[kind] ?? []
    return sources.map(source => ({
      key: `${entry.id}:${source.name}`,
      pluginId: entry.id,
      source: source.name,
      title: source.title,
      description: source.description ?? '',
      pluginName: manifest.name,
      brand: manifest.brand,
      instances,
      fields: source.fields ?? [],
    }))
  })
}

/**
 * One page of a source's projects. Packages may ignore a search, so the picker filters what it
 * loaded instead of sending one.
 */
export function projectsPath(pluginId: string, source: string, instance: string, page: number) {
  const params = new URLSearchParams({ source, instance, page: String(page) })
  return `/plugins/${encodeURIComponent(pluginId)}/projects?${params}`
}

/** Whether a project matches every word typed, in its name, identifier, description or badges. */
export function projectMatches(project: PluginProject, query: string) {
  const words = searchText(query).trim().split(/\s+/).filter(Boolean)
  const text = searchText([project.name, project.id, project.description, ...project.badges].join(' '))
  return words.every(word => text.includes(word))
}

const relative = new Intl.RelativeTimeFormat('en', { numeric: 'auto' })

/** "Updated 3 days ago", or '' without a valid date. */
export function updatedLabel(value: string | null | undefined, now = Date.now()) {
  const time = value ? Date.parse(value) : Number.NaN
  if (Number.isNaN(time))
    return ''
  const seconds = (time - now) / 1000
  for (const [unit, size] of [['year', 31536000], ['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600], ['minute', 60]] as const) {
    if (Math.abs(seconds) >= size)
      return `Updated ${relative.format(Math.round(seconds / size), unit)}`
  }

  return 'Updated just now'
}
