import type {
  PluginConnection,
  PluginEntry,
  SettingField,
} from './plugins'

/** What connecting an instance takes: a sign-in, a pasted token, or an app registered first. */
export type SetupEffort = 'signin' | 'token' | 'app'
export type SetupRisk = 'low' | 'medium' | 'high'

/** Something the owner does on the service's site, ticked off in the assistant. */
export interface SetupTask {
  title: string
  text?: string
  url?: string
  link?: string
  /** A value Cairn shows for the owner to copy into the service. */
  copy?: 'callbackUrl'
}

/** A named combination of a step's switches, e.g. "Read only". */
export interface SetupPreset {
  id: string
  title: string
  description?: string
  risk?: SetupRisk
  /** Switches turned on; the step's other switches are turned off. */
  values?: Record<string, boolean>
}

export interface SetupStep {
  id: string
  kind: 'settings' | 'credential' | 'people' | 'agents'
  title: string
  description?: string
  settings?: string[]
  /** The same values serve every instance; a new one can reuse them from another. */
  shared?: boolean
  checklist?: SetupTask[]
  presets?: SetupPreset[]
  warning?: string
}

export interface PluginSetup {
  effort?: SetupEffort | null
  summary?: string
  /** A string setting naming a new instance once known, e.g. the verified mailbox. */
  nameFrom?: string | null
  steps: SetupStep[]
}

/** The setup resolved by the installation; invalid packages have no setup. */
export function pluginSetup(entry: Pick<PluginEntry, 'setup'>): PluginSetup {
  return entry.setup ?? { steps: [] }
}

/** The catalogue's words for what connecting takes. */
export const effortLabels: Record<SetupEffort, string> = {
  signin: 'Sign in',
  token: 'Paste a token',
  app: 'Needs your own app',
}

export const riskLabels: Record<SetupRisk, string> = {
  low: 'Low risk',
  medium: 'Medium risk',
  high: 'High risk',
}

/** The step's fields, in the step's order. */
export function stepFields(step: SetupStep, fields: SettingField[]) {
  return (step.settings ?? []).flatMap(key => fields.filter(field => field.key === key))
}

/** The preset the step's switches match, or '' when they were fine-tuned. */
export function matchingPreset(step: SetupStep, fields: SettingField[], values: Record<string, unknown>) {
  const switches = stepFields(step, fields).filter(field => field.type === 'boolean')
  return step.presets?.find(preset => switches.every(field => (values[field.key] === true) === (preset.values?.[field.key] === true)))?.id ?? ''
}

/** The step's switches as the preset sets them. */
export function presetValues(step: SetupStep, fields: SettingField[], preset: SetupPreset) {
  return Object.fromEntries(stepFields(step, fields)
    .filter(field => field.type === 'boolean')
    .map(field => [field.key, preset.values?.[field.key] === true]))
}

/** One app (or other shared setup) already saved, and the instances using it. */
export interface SharedSource {
  /** The instance the installation copies the values from. */
  id: string
  names: string[]
  /** The visible shared values, e.g. the OAuth client ID, to tell apps apart. */
  label: string
}

/**
 * Saved instances whose required shared values are complete. Secret values never reach
 * clients, so sources with secrets remain individually selectable even if their labels match.
 */
export function sharedSources(steps: SetupStep[], fields: SettingField[], connections: PluginConnection[], except?: string): SharedSource[] {
  const shared = steps.filter(step => step.shared).flatMap(step => stepFields(step, fields))
  if (!shared.length)
    return []
  const sources: SharedSource[] = []
  for (const connection of connections) {
    if (connection.id === except)
      continue
    const complete = shared.every(field => field.type === 'secret'
      ? !field.required || connection.secretKeys?.includes(field.key)
      : !field.required || (connection.settings[field.key] !== undefined && connection.settings[field.key] !== null && connection.settings[field.key] !== ''))
    if (!complete)
      continue
    const label = shared.filter(field => field.type !== 'secret')
      .map(field => String(connection.settings[field.key] ?? ''))
      .filter(Boolean)
      .join(' · ')
    sources.push({ id: connection.id, names: [connection.name], label })
  }

  return sources
}

/** "4812…tent.com": long identifiers stay recognisable without filling a line. */
export function shortValue(value: string, keep = 10) {
  return value.length > keep * 2 + 1 ? `${value.slice(0, keep)}…${value.slice(-keep)}` : value
}
