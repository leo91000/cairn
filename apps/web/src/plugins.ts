import type { Agent } from '../../../packages/contracts/contracts'
import type { IconName } from './icons'
import {
  BrandDiscord,
  BrandLinear,
  BrandOnePassword,
  BrandSlack,
  BrandTelegram,
} from './icons'

export interface PluginPackage {
  id: string
  name: string
  version: string
  description: string
  capabilities: string[]
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
  status: 'working' | 'replied' | 'failed'
  createdAt: number
}

export interface DiscordConnection {
  id: string
  name: string
  enabled: boolean
  guildId: string
  guildName: string
  channelId: string
  channelName: string
  userIds: string[]
  people: Label[]
  accessMode: 'allowlist' | 'requests'
  bot: Label | null
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
  channels?: (Label & { canRead: boolean, canReply: boolean })[]
  users?: Label[]
}

export const brands: Record<string, { icon: IconName, color: string }> = {
  discord: { icon: BrandDiscord, color: '#5865F2' },
  onepassword: { icon: BrandOnePassword, color: '#1A285F' },
  slack: { icon: BrandSlack, color: '#4A154B' },
  telegram: { icon: BrandTelegram, color: '#26A5E4' },
  linear: { icon: BrandLinear, color: '#5E6AD2' },
}

export function connectionState(connection: DiscordConnection) {
  if (!connection.enabled)
    return { label: 'Paused', tone: 'muted' } as const
  return ({
    connected: { label: 'Online', tone: 'ok' },
    connecting: { label: 'Connecting…', tone: 'muted' },
    needs_review: { label: 'Needs review', tone: 'coral' },
  } as const)[connection.status.state as 'connected'] ?? { label: 'Offline', tone: 'coral' } as const
}

export function personName(connection: DiscordConnection, id: string) {
  return connection.people.find(person => person.id === id)?.name || `User ${id.slice(-4)}`
}

/** External people may only reach a dedicated agent: one project at most, no installation access. */
export function eligibleAgent(agent: Agent) {
  return agent.access.installation !== true && Array.isArray(agent.access.projects) && agent.access.projects.length <= 1
}
