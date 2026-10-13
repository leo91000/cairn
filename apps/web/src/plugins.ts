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
  Globe,
  HardDrive,
  KeyRound,
  MessageSquare,
} from './icons'

export interface SettingField {
  key: string
  title: string
  type: 'string' | 'number' | 'boolean'
  description?: string
  required?: boolean
  hidden?: boolean
  maxLength?: number
  minimum?: number
  maximum?: number
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
  userIds: string[]
  people: Label[]
  accessMode: 'allowlist' | 'requests'
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
  'onepassword.read': [KeyRound, 'Secrets'],
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
