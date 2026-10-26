import type { Agent } from '../../../packages/contracts/contracts'
import type { PluginConnection } from '../../../packages/contracts/generated/typescript/PluginConnection'
import type { PluginOption as GeneratedPluginOption } from '../../../packages/contracts/generated/typescript/PluginOption'
import type { IconName } from './icons'
import type { PluginSetup, SetupRisk } from './plugin-setup'
import {
  BrandLinear,
  BrandSlack,
  BrandTelegram,
  Cable,
  CirclePause,
  Clock,
  Cpu,
  Globe,
  HardDrive,
  Inbox,
  KeyRound,
  LayoutPanelTop,
  MessageSquare,
  Terminal,
  Wrench,
} from './icons'

export type FieldOption = Pick<GeneratedPluginOption, 'value' | 'label'> & Partial<GeneratedPluginOption>

/** One field of a declarative plugin form (instance settings or a view's form). */
export interface SettingField {
  key: string
  title: string
  type: 'string' | 'text' | 'number' | 'boolean' | 'secret' | 'select' | 'multiselect' | 'url'
  description?: string
  placeholder?: string
  section?: string
  required?: boolean
  hidden?: boolean
  maxLength?: number
  minimum?: number
  maximum?: number
  pattern?: string
  options?: FieldOption[]
  /** Choices come from the package's form handler while the owner edits. */
  dynamic?: boolean
  /** Changing this field asks the package's form handler again. */
  refresh?: boolean
  /** How much a switch lets agents do. */
  risk?: SetupRisk
  default?: unknown
}

export type { PluginActivity as Activity } from '../../../packages/contracts/generated/typescript/PluginActivity'

export interface PluginArgument {
  name: string
  flag?: string
  positional?: boolean
  required?: boolean
  description?: string
}

export interface PluginCommand {
  name: string
  description?: string
  output?: 'json' | 'text' | 'secret'
  arguments?: PluginArgument[]
}

export interface Manifest {
  id: string
  name: string
  version: string
  description: string
  apiVersion: number
  capabilities: string[]
  hosts?: string[]
  credential?: {
    label: string
    header: string
    prefix?: string
    /** The owner can sign in on the service's page instead of pasting a token. */
    deviceFlow?: {
      authorizationUrl: string
      tokenUrl: string
      clientId: string
      scopes?: string[]
    }
    oauth?: {
      authorizationUrl: string
      tokenUrl: string
      revocationUrl?: string
      clientIdSetting: string
      clientSecretSetting?: string
      scopes?: string[]
      scopeSettings?: Record<string, string[]>
      authorizationParameters?: Record<string, string>
    }
  }
  settings?: SettingField[]
  instances?: { singular: string, plural: string }
  brand?: { color: string, path?: string }
  programs?: {
    name: string
    path?: string
    installer?: string
    description?: string
  }[]
  agentAccess?: { description?: string }
  commands?: PluginCommand[]
  tools?: { name: string, description: string }[]
  instructions?: string
  instanceView?: boolean
  instanceTest?: boolean
  instanceForm?: boolean
  /** Where its enabled instances import projects from. */
  projectSources?: PluginSource[]
  /** Where its enabled instances import skills from, with the location's fields. */
  skillSources?: (PluginSource & { fields: SettingField[] })[]
  /** Conditions granted agents can wait for; agents use them, the owner never edits them. */
  conditions?: { name: string, description: string, project?: boolean }[]
  /** What granted agents' runs receive from its instance, such as git credentials. */
  runCredentials?: Record<string, unknown>
  secretPatterns?: string[]
  /** What connecting takes and the steps the owner follows, as the package declares them. */
  setup?: Partial<PluginSetup>
}

/** A named source a package offers, such as GitHub repositories. */
export interface PluginSource {
  name: string
  title: string
  description?: string
}

export interface ProgramRelease {
  version: string
  url: string
  sha256?: string
  format: 'zip' | 'binary'
  executable: string
}

export interface InstalledProgram {
  release: ProgramRelease
  archiveSha256: string
  binarySha256: string
}

export interface ProgramState {
  name: string
  installer: string
  installed?: InstalledProgram | null
  previous?: InstalledProgram | null
  candidate?: ProgramRelease | null
  digest?: string | null
}

export interface PackageVersion {
  version: string
  digest?: string
  /** The package's Luau files, `main.luau` first. */
  files?: { path: string, bytes: number }[]
  manifest?: Manifest
  /** The steps the owner follows, resolved by the installation. */
  setup?: PluginSetup
  approved?: boolean
  error?: string
}

/** A bundled or installed package and the owner's approval state. */
export interface PluginEntry {
  id: string
  builtin: boolean
  status: 'active' | 'update' | 'needs_approval' | 'invalid'
  active: string | null
  manifest: Manifest | null
  /** The steps resolved by the installation; null for invalid packages. */
  setup?: PluginSetup | null
  versions: PackageVersion[]
}

export interface Catalog {
  packages: PluginEntry[]
  capabilities: { name: string, description: string }[]
  connections: PluginConnection[]
}

export type { PluginConnection } from '../../../packages/contracts/generated/typescript/PluginConnection'
export type { PluginFormResult as FormResult } from '../../../packages/contracts/generated/typescript/PluginFormResult'
export type { PluginLabel as Label } from '../../../packages/contracts/generated/typescript/PluginLabel'
export type { PluginRequest as AccessRequest } from '../../../packages/contracts/generated/typescript/PluginRequest'

/** How each host capability reads in the catalogue. */
export const capabilityLabels: Record<string, [IconName, string]> = {
  'socket': [Cable, 'Live connection'],
  'inbound': [Inbox, 'Receives messages'],
  'conversations.submit': [MessageSquare, 'Conversations'],
  'conversations.control': [CirclePause, 'Answer and stop'],
  'http': [Globe, 'Web requests'],
  'storage': [HardDrive, 'Storage'],
  'timers': [Clock, 'Timers'],
  'exec': [Cpu, 'Runs programs'],
}

/** Risk pills: low reads as safe, high as a warning. */
export const riskTones: Record<SetupRisk, string> = {
  low: 'bg-[light-dark(#dff3ee,#163a33)] text-success',
  medium: 'bg-warning-surface text-warning',
  high: 'bg-danger-surface text-danger',
}

/** What a package gives agents, for catalogue chips. */
export function contributions(manifest: Manifest | null | undefined): [IconName, string][] {
  if (!manifest)
    return []
  return [
    ...(manifest.commands?.length ? [[Terminal, 'CLI commands'] as [IconName, string]] : []),
    ...(manifest.tools?.length ? [[Wrench, 'Agent tools'] as [IconName, string]] : []),
    ...(manifest.agentAccess ? [[KeyRound, 'Per-agent grants'] as [IconName, string]] : []),
    ...(manifest.instanceView ? [[LayoutPanelTop, 'Dashboard'] as [IconName, string]] : []),
  ]
}

/** Packages that react to people, connections or timers keep a worker running. */
export function needsWorker(manifest: Manifest | null | undefined) {
  return ['socket', 'inbound', 'conversations.submit', 'conversations.control', 'timers']
    .some(name => manifest?.capabilities.includes(name))
}

/** Packages whose instances let people outside Cairn talk to an agent. */
export function delegates(manifest: Manifest | null | undefined) {
  return manifest?.capabilities.includes('conversations.submit') ?? false
}

/** "Service account" / "Service accounts", or the generic wording. */
export function instanceNoun(manifest: Manifest | null | undefined, count = 1) {
  if (manifest?.instances)
    return count === 1 ? manifest.instances.singular : manifest.instances.plural
  return count === 1 ? 'Instance' : 'Instances'
}

/** Command-line usage, as agents type it. */
export function commandUsage(plugin: string, command: PluginCommand) {
  const parts = (command.arguments ?? []).map((argument) => {
    const part = argument.flag ? `${argument.flag} <${argument.name}>` : `<${argument.name}>`
    return argument.required ? part : `[${part}]`
  })
  return ['cairn plugin', plugin, command.name, ...parts].join(' ')
}

/** Marks of plugins announced in the catalogue, not available yet. Packages bring their own. */
export const upcoming: Record<string, { icon: IconName, color: string }> = {
  slack: { icon: BrandSlack, color: '#4A154B' },
  telegram: { icon: BrandTelegram, color: '#26A5E4' },
  linear: { icon: BrandLinear, color: '#5E6AD2' },
}

export function connectionState(connection: PluginConnection) {
  if (!connection.enabled)
    return { label: 'Paused', tone: 'muted' } as const
  return ({
    ready: { label: 'Ready', tone: 'ok' },
    connected: { label: 'Online', tone: 'ok' },
    connecting: { label: 'Connecting…', tone: 'muted' },
    needs_review: { label: 'Needs review', tone: 'coral' },
  } as const)[connection.status.state as 'connected'] ?? { label: 'Offline', tone: 'coral' } as const
}

export function personName(connection: PluginConnection, id: string) {
  return connection.people.find(person => person.id === id)?.name || `User ${id.slice(-4)}`
}

/**
 * What the people reaching an agent through a bot could do with its access, for the
 * owner's warning. Nothing is refused: the owner chooses who may reach which agent.
 * For anyone in a channel, writing, MCP and plugin access matter as well.
 */
export function delegationRisks(agent: Agent, connections: Pick<PluginConnection, 'agentIds'>[], everyone: boolean, draftGrants: string[] = []) {
  const risks: string[] = []
  if (agent.access.installation === true)
    risks.push('manage your whole installation')
  if (!Array.isArray(agent.access.projects))
    risks.push('open every project')
  else if (agent.access.projects.length > 1)
    risks.push(`open ${agent.access.projects.length} projects`)
  if (!everyone)
    return risks
  if (agent.access.sandbox !== 'read-only')
    risks.push('change files and run commands')
  if (!Array.isArray(agent.access.mcps) || agent.access.mcps.length)
    risks.push('use its MCP connections')
  if (draftGrants.includes(agent.id) || connections.some(connection => connection.agentIds?.includes(agent.id)))
    risks.push('use its plugins, such as GitHub or 1Password')
  return risks
}

/** `a, b and c`. */
export function listed(items: string[]) {
  return items.length > 1 ? `${items.slice(0, -1).join(', ')} and ${items.at(-1)}` : items[0] ?? ''
}
