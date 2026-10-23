import type {
  Manifest,
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

/**
 * The installation resolves the setup (`entry.setup`). An older installation sends only the
 * manifest: resolve it the same way, declared steps first, else the default ones.
 */
export function pluginSetup(entry: Pick<PluginEntry, 'manifest' | 'setup'>): PluginSetup {
  if (entry.setup)
    return { ...entry.setup, steps: entry.setup.steps ?? [] }
  if (!entry.manifest)
    return { steps: [] }
  const derived = defaultSetup(entry.manifest)
  const declared = entry.manifest.setup
  return {
    ...declared,
    effort: declared?.effort ?? derived.effort,
    steps: declared?.steps?.length ? declared.steps : derived.steps,
  }
}

function defaultSetup(manifest: Manifest): PluginSetup {
  const credential = manifest.credential
  const visible = (manifest.settings ?? []).filter(field => !field.hidden).map(field => field.key)
  const settings: SetupStep[] = visible.length
    ? [{
        id: 'settings',
        kind: 'settings',
        title: 'Settings',
        settings: visible,
      }]
    : []
  const signIn: SetupStep[] = credential
    ? [{ id: 'credential', kind: 'credential', title: credential.oauth || credential.deviceFlow ? `Sign in with ${manifest.name}` : credential.label }]
    : []
  const steps = credential?.oauth ? [...settings, ...signIn] : [...signIn, ...settings]
  if (manifest.capabilities.includes('conversations.submit'))
    steps.push({ id: 'people', kind: 'people', title: 'Who can talk to it' })
  if (manifest.agentAccess)
    steps.push({ id: 'agents', kind: 'agents', title: 'Agents' })
  const secret = (manifest.settings ?? []).some(field => field.type === 'secret' && field.required)
  const effort: SetupEffort | null = credential?.oauth
    ? 'app'
    : credential?.deviceFlow ? 'signin' : credential || secret ? 'token' : null
  return { effort, steps }
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
 * Saved instances whose shared steps are complete, one per distinct set of values: reusing
 * any of them copies the same app.
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
      ? connection.secretKeys?.includes(field.key)
      : !field.required || (connection.settings[field.key] !== undefined && connection.settings[field.key] !== ''))
    if (!complete)
      continue
    const label = shared.filter(field => field.type !== 'secret')
      .map(field => String(connection.settings[field.key] ?? ''))
      .filter(Boolean)
      .join(' · ')
    const same = sources.find(source => source.label === label)
    if (same)
      same.names.push(connection.name)
    else
      sources.push({ id: connection.id, names: [connection.name], label })
  }

  return sources
}

/** "4812…tent.com": long identifiers stay recognisable without filling a line. */
export function shortValue(value: string, keep = 10) {
  return value.length > keep * 2 + 1 ? `${value.slice(0, keep)}…${value.slice(-keep)}` : value
}
