import type { ChatView } from '../../../packages/contracts/chats'
import { MAIN_AGENT_ID } from '../../../packages/contracts/constants'

// Which agents an account may use (ADR-0075). The server enforces it; the apps only avoid
// offering what would be refused. `null` means every agent: the owner, or not loaded yet.
export type UsableAgents = readonly string[] | null

export function canUseAgent(usable: UsableAgents, agentId: string | null | undefined) {
  return usable === null || (!!agentId && usable.includes(agentId))
}

/**
 * A conversation is directed through its agent. A delegated task may also be directed
 * through the agent of its requesting conversation, while that conversation exists.
 */
export function canDirectChat(usable: UsableAgents, chat: Pick<ChatView, 'agentId' | 'requestedBy'>, chats: readonly Pick<ChatView, 'id' | 'agentId'>[]) {
  if (canUseAgent(usable, chat.agentId))
    return true
  const requester = chat.requestedBy?.chatId
  const requesting = requester ? chats.find(item => item.id === requester) : undefined
  return !!requesting && canUseAgent(usable, requesting.agentId)
}

/**
 * Who may direct a conversation the viewer can only follow: people with access to its agent and,
 * for a delegated task whose requesting conversation still exists, to that conversation's agent.
 */
export function readOnlyNotice(chat: Pick<ChatView, 'agentName' | 'requestedBy'>, chats: readonly Pick<ChatView, 'id' | 'agentName'>[]) {
  const own = chat.agentName || 'its agent'
  const requester = chat.requestedBy?.chatId
  const requesting = requester ? chats.find(item => item.id === requester) : undefined
  const who = requesting
    ? `Only people with access to ${own} or to ${requesting.agentName || 'the agent of the conversation that delegated it'}`
    : `Only people with access to ${own}`
  return `${who} can continue it or answer its questions, and only people with access to ${own} can delete it. You can still pause or stop its work. Ask the installation owner for access.`
}

/** The agent a new conversation starts with: Main when allowed, otherwise the first usable one. */
export function defaultAgent(usable: UsableAgents, agents: readonly { id: string }[]) {
  if (canUseAgent(usable, MAIN_AGENT_ID))
    return MAIN_AGENT_ID
  return agents.find(agent => canUseAgent(usable, agent.id))?.id ?? ''
}

export interface Member { id: string, email: string }

export interface AccessRow {
  accountId: string
  email: string
  // A former member still holds the entry until the next membership check.
  known: boolean
  allowed: boolean
}

/** Every member, with whether they may use the agent; leftover entries come last. */
export function accessRows(members: readonly Member[], allowed: readonly string[]): AccessRow[] {
  const rows = members.map(member => ({
    accountId: member.id,
    email: member.email,
    known: true,
    allowed: allowed.includes(member.id),
  }))
  const leftovers = allowed.filter(account => !members.some(member => member.id === account))
  return [...rows, ...leftovers.map(accountId => ({
    accountId,
    email: 'Former member',
    known: false,
    allowed: true,
  }))]
}

/**
 * Applies one member's change from a response and keeps the other rows as shown, so responses to
 * several quick changes cannot undo each other when they arrive out of order.
 */
export function withAccess(allowed: readonly string[], accountId: string, accounts: readonly string[]) {
  if (!accounts.includes(accountId))
    return allowed.filter(account => account !== accountId)
  return allowed.includes(accountId) ? [...allowed] : [...allowed, accountId]
}

export function accessSummary(allowed: number) {
  if (!allowed)
    return { short: 'Only you', long: 'Only you' }
  const people = `${allowed} ${allowed === 1 ? 'member' : 'members'}`
  return { short: `You + ${allowed}`, long: `You and ${people}` }
}
