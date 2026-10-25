import type { Agent } from '../../../packages/contracts/contracts'
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
  /** Choices are looked up by query/cursor rather than assumed to be exhaustive. */
  remote?: boolean
  /** Changing this field asks the package's form handler again. */
  refresh?: boolean
  /** How much a switch lets agents do. */
  risk?: SetupRisk
  default?: unknown
}

/** What a package's `form` handler returns while the owner edits an instance. */
export interface FormResult {
  options: Record<string, FieldOption[]>
  values: Record<string, unknown>
  errors: Record<string, string>
  view: any[]
  pages?: Record<string, { nextCursor: string | null }>
  /** Added by the host after validating a choice-only result. */
  lookup?: FormLookup
}

export interface FormLookup {
  field: string
  query: string
  cursor?: string
}

/** Omitted or null guidance is a successful form with nothing to add. */
export function formResult(value: Partial<FormResult> | null): FormResult {
  return {
    options: value?.options ?? {},
    values: value?.values ?? {},
    errors: value?.errors ?? {},
    view: value?.view ?? [],
    ...(value?.pages && { pages: value.pages }),
    ...(value?.lookup && { lookup: value.lookup }),
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
  resources?: PluginResourceCollection[]
  operations?: PluginOperation[]
  oauth?: PluginOAuthProvider[]
  webhooks?: { name: string }[]
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
  form?: boolean
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
  /** The host supports owner setup drafts; absent on earlier installations. */
  setupDrafts?: boolean
  resourceGrantCounts?: Record<string, number>
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
  return Boolean(manifest?.capabilities.includes('conversations.submit') && !manifest.resources?.some(resource => resource.delegation))
}

/** "Service account" / "Service accounts", or the generic wording. */
export function instanceNoun(manifest: Pick<Manifest, 'instances'> | null | undefined, count = 1) {
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

export function connectionState(connection: Pick<PluginConnection, 'enabled' | 'status'>) {
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
export function delegationRisks(agent: Agent, connections: (Pick<PluginConnection, 'agentIds'> & { resourceAgentIds?: string[] })[], everyone: boolean, draftGrants: string[] = []) {
  const risks: string[] = []
  if (agent.access.installation === true)
    risks.push('manage your whole installation')
  if (agent.access.delegation === true)
    risks.push('delegate work to other coding agents and models')
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
  if (draftGrants.includes(agent.id) || connections.some(connection => (connection.agentIds?.includes(agent.id) || connection.resourceAgentIds?.includes(agent.id))))
    risks.push('use its plugins, such as GitHub or 1Password')
  return risks
}

/** Configuration never grants the bot's external sender native host authority. */
export function externalBotExecutionWarning(agent: Agent) {
  return agent.access.backend === 'native'
    ? 'External bot messages cannot start work with a native agent because they do not carry a verified Cairn host identity. Choose an isolated agent for this bot.'
    : ''
}

/** `a, b and c`. */
export function listed(items: string[]) {
  return items.length > 1 ? `${items.slice(0, -1).join(', ')} and ${items.at(-1)}` : items[0] ?? ''
}

export interface RouteEligibility {
  agentId: string
  eligible: boolean
  pluginAccess?: boolean
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
  summary?: { hourStartedAt: number, messagesThisHour: number, pendingRequests: number } | null
}
