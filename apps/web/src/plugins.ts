import type { Agent } from '../../../packages/contracts/contracts'
import type { IconName } from './icons'
import {
  Bot,
  BrandDiscord,
  BrandLinear,
  BrandOnePassword,
  BrandSlack,
  BrandTelegram,
  CirclePause,
  Clock,
  Cpu,
  Globe,
  HardDrive,
  KeyRound,
  LayoutPanelTop,
  MessageSquare,
  Terminal,
  Wrench,
} from './icons'

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
  options?: { value: string, label: string }[]
  default?: unknown
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
  programs?: { name: string, path: string, description?: string }[]
  agentAccess?: { description?: string }
  commands?: PluginCommand[]
  tools?: { name: string, description: string }[]
  instructions?: string
  instanceView?: boolean
  instanceTest?: boolean
}

export interface PackageVersion {
  version: string
  digest?: string
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
  status: { state: string }
  usage: { hour: number }
  requests: AccessRequest[]
  activity: Activity[]
}

export interface Discovery {
  bot: Label & { username: string }
  applicationId: string
  inviteUrl: string
  guilds: Label[]
  channels?: (Label & { canRead: boolean, canReply: boolean, canThread?: boolean })[]
  users?: Label[]
}

/** Discord connections keep their scope and labels in plugin settings. */
export type DiscordConnection = PluginConnection & {
  settings: {
    guildId: string
    guildName?: string
    channelId: string
    channelName?: string
    botId?: string
    botName?: string
  }
}

/** How each host capability reads in the catalogue. */
export const capabilityLabels: Record<string, [IconName, string]> = {
  'transport.discord': [Bot, 'Discord bot'],
  'conversations.submit': [MessageSquare, 'Conversations'],
  'conversations.control': [CirclePause, 'Answer and stop'],
  'http': [Globe, 'Web requests'],
  'storage': [HardDrive, 'Storage'],
  'timers': [Clock, 'Timers'],
  'exec': [Cpu, 'Runs programs'],
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

/** Packages that react to people, transports or timers keep a worker running. */
export function needsWorker(manifest: Manifest | null | undefined) {
  return ['transport.discord', 'conversations.submit', 'conversations.control', 'timers']
    .some(name => manifest?.capabilities.includes(name))
}

/** "Service account" / "Service accounts", or the generic wording. */
export function instanceNoun(manifest: Manifest | null | undefined, count = 1) {
  if (manifest?.instances)
    return count === 1 ? manifest.instances.singular : manifest.instances.plural
  if (manifest?.capabilities.includes('transport.discord'))
    return count === 1 ? 'Bot' : 'Bots'
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

export const brands: Record<string, { icon: IconName, color: string }> = {
  discord: { icon: BrandDiscord, color: '#5865F2' },
  onepassword: { icon: BrandOnePassword, color: '#1A285F' },
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
