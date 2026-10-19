import type { Agent } from '../../../packages/contracts/contracts'
import type { IconName } from './icons'
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

export interface FieldOption {
  value: string
  label: string
  /** Shown under the choice, e.g. why it can't be picked. */
  description?: string
  disabled?: boolean
}

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
  default?: unknown
}

/** What a package's `form` handler returns while the owner edits an instance. */
export interface FormResult {
  options: Record<string, FieldOption[]>
  values: Record<string, unknown>
  errors: Record<string, string>
  view: any[]
}

/** Omitted or null guidance is a successful form with nothing to add. */
export function formResult(value: Partial<FormResult> | null): FormResult {
  return {
    options: value?.options ?? {},
    values: value?.values ?? {},
    errors: value?.errors ?? {},
    view: value?.view ?? [],
  }
}

/** Errors without an inline field still need an explanation in the form. */
export function unshownFormErrors(manifest: Manifest, errors: Record<string, string> = {}) {
  const inline = new Set((manifest.settings ?? []).filter(field => !field.hidden).map(field => field.key))
  inline.add('form')
  if (manifest.credential)
    inline.add('credential')
  return Object.entries(errors).filter(([key]) => !inline.has(key)).map(([, message]) => message)
}

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
  credential?: { label: string, header: string, prefix?: string }
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
  resources?: PluginResourceCollection[]
  operations?: PluginOperation[]
  oauth?: PluginOAuthProvider[]
  webhooks?: { name: string }[]
}

export type PluginAccessProfile = 'read' | 'contribute' | 'manage'

export interface PluginOperation {
  name: string
  collection: string
  profile: PluginAccessProfile
  scopes?: string[]
}

export interface PluginResourceGrant {
  id: string
  resourceId: string
  agentId: string
  profile: PluginAccessProfile | null
  operations: string[]
  deniedOperations: string[]
  scopes: Record<string, string[]>
  revision: string
}

/** A preview of this rule only; other saved rules may independently grant access. */
export function resourceRuleOperations(operations: PluginOperation[], rule: Pick<PluginResourceGrant, 'profile' | 'operations' | 'deniedOperations' | 'scopes'>) {
  const levels = { read: 1, contribute: 2, manage: 3 }
  return operations.filter(operation => !rule.deniedOperations.includes(operation.name)
    && ((rule.profile && levels[rule.profile] >= levels[operation.profile]) || rule.operations.includes(operation.name))
    && Object.keys(rule.scopes).every(scope => operation.scopes?.includes(scope)))
}

export interface PluginResourceCollection {
  name: string
  title: string
  singular: string
  parent?: string
  delegation?: boolean
  settings?: SettingField[]
}

export interface PluginResource {
  id: string
  connectionId: string
  collection: string
  parentId?: string | null
  name: string
  enabled: boolean
  revision: string
  settings: Record<string, any>
  secretKeys: string[]
}

export interface PluginOAuthProvider {
  name: string
  title: string
  collection: string
  scopes: string[]
  personal?: { subject: string }
}

export interface PluginOAuthStatus {
  status: 'disconnected' | 'authorized' | 'reconnect' | 'pending' | 'denied'
  scopes?: string[]
  requestedScopes?: string[]
  expiresAt?: number | null
  subject?: string
}

export interface PluginOAuthConsent {
  state: string
  url: string
  redirectUri?: string
  expiresAt: number
}

export interface PluginPersonalAuthorization {
  id: string
  pluginId: string
  pluginName: string
  instanceId: string
  instanceName: string
  collection: string
  resourceId: string
  resourceName: string
  providerName: string
  title: string
  scopes: string[]
  enabled: boolean
  authorization: PluginOAuthStatus
}

export function personalAuthorizationPath(item: PluginPersonalAuthorization) {
  return `/plugins/${encodeURIComponent(item.pluginId)}/connections/${encodeURIComponent(item.instanceId)}/resources/${encodeURIComponent(item.collection)}/${encodeURIComponent(item.resourceId)}/oauth/${encodeURIComponent(item.providerName)}`
}

export interface PluginSetupDraft {
  id: string
  pluginId: string
  name: string
  status: 'setup' | 'review_needed'
  settings: Record<string, any>
  secretKeys: string[]
  credential: boolean
  revision: string
  createdAt: number
  expiresAt: number
}

export interface PluginPage<T> {
  items: T[]
  nextCursor?: string | null
}

/** Configuration requiring stable callbacks or resources before activation. */
export function hasSetup(manifest: Manifest | null | undefined) {
  return Boolean(manifest?.resources?.length || manifest?.oauth?.length || manifest?.webhooks?.length)
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
  versions: PackageVersion[]
}

export interface Catalog {
  packages: PluginEntry[]
  capabilities: { name: string, description: string }[]
  connections: PluginConnection[]
}

export interface Label {
  id: string
  name: string
}

export interface AccessRequest {
  id: string
  userId: string
  name: string
  mentions: number
  updatedAt: number
}

export interface Activity {
  id: string
  chatId: string
  userId: string
  name: string
  preview: string
  status: 'working' | 'replied' | 'failed' | 'stopped'
  createdAt: number
}

export interface PluginConnection {
  id: string
  pluginId: string
  name: string
  enabled: boolean
  settings: Record<string, any>
  /** Agents granted the package's commands and tools on this instance. */
  agentIds?: string[]
  /** Secret fields saved in the vault; values never reach the browser. */
  secretKeys?: string[]
  userIds: string[]
  people: Label[]
  accessMode: 'allowlist' | 'requests' | 'everyone'
  maxPerPersonPerHour?: number
  agentId: string
  projectId: string | null
  maxMessagesPerHour: number
  /** Live state, with the package's own message (e.g. who it is signed in as). */
  status: { state: string, message?: string | null }
  usage: { hour: number }
  requests: AccessRequest[]
  activity: Activity[]
}

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
  'resources': [HardDrive, 'Resources'],
  'oauth': [KeyRound, 'Account authorization'],
  'webhooks': [Inbox, 'Receives web requests'],
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
  return Boolean(manifest?.capabilities.includes('conversations.submit') && !manifest.resources?.some(resource => resource.delegation))
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

/** External people may only reach a dedicated agent: one project at most, no installation access. */
export function eligibleAgent(agent: Agent) {
  return agent.access.installation !== true && Array.isArray(agent.access.projects) && agent.access.projects.length <= 1
}

/**
 * Why an agent can't serve anyone in a channel, or '' when it can. Mirrors the server:
 * read-only sandbox, one project at most, no installation, GitHub, MCP or plugin grants.
 */
export function publicProblem(agent: Agent, connections: Pick<PluginConnection, 'agentIds'>[]) {
  if (!eligibleAgent(agent))
    return 'Too broad'
  if (agent.access.sandbox !== 'read-only')
    return 'Not read-only'
  if (agent.access.github)
    return 'Has GitHub'
  if (!Array.isArray(agent.access.mcps) || agent.access.mcps.length)
    return 'Has MCPs'
  if (connections.some(connection => connection.agentIds?.includes(agent.id)))
    return 'Has plugin access'
  return ''
}

export interface RouteEligibility {
  agentId: string
  eligible: boolean
  reason?: string
}

export interface PluginRoute {
  id: string
  resourceId: string
  name: string
  enabled: boolean
  match: Record<string, string>
  agentId: string
  projectId: string | null
  accessMode: 'allowlist' | 'requests' | 'everyone'
  userIds: string[]
  people: { id: string, name: string }[]
  maxMessagesPerHour: number
  maxPerPersonPerHour: number
  revision: string
}
