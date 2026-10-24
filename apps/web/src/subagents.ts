import type { ComputedRef, InjectionKey } from 'vue'
import type { ChatView } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import type { ActivityArtifact, ActivityEntry, ActivitySubagent } from './activity'
import { teamEntries } from './activity'
import { identityColor, liveElapsed } from './signal'

export type SubagentState = ActivitySubagent['state']

export const subagentLabels: Record<SubagentState, string> = {
  running: 'Running',
  done: 'Done',
  failed: 'Failed',
  stopped: 'Stopped',
}

export const subagentPills: Record<SubagentState, string> = {
  running: 'bg-soft text-accent',
  done: 'bg-variant text-success',
  failed: 'bg-coral-soft text-coral',
  stopped: 'bg-variant text-muted',
}

export const subagentDots: Record<SubagentState, string> = {
  running: 'subagent-pulse bg-accent',
  done: 'bg-success',
  failed: 'bg-coral',
  stopped: 'bg-subtle',
}

/** Opens a subagent's full activity beside the conversation, when the page offers it. */
export const openSubagentKey: InjectionKey<(id: string, opener?: HTMLElement) => void> = Symbol('open-subagent')

/** The conversations the page knows, by id: delegated tasks follow theirs. */
export const delegatedChatsKey: InjectionKey<ComputedRef<Map<string, ChatView>>> = Symbol('delegated-chats')

/** A delegated task, as its conversation shows it now. A missing conversation was deleted. */
function delegatedState(chat: ChatView | undefined): SubagentState {
  if (!chat || (chat.lifecycle && chat.lifecycle !== 'active'))
    return 'stopped'
  if (chat.status === 'failed' || chat.status === 'interrupted')
    return 'failed'
  if (chat.paused || chat.status === 'cancelled')
    return 'stopped'
  if (chat.pendingQuestions > 0 || chat.status === 'running' || chat.status === 'queued' || chat.wakeup)
    return 'running'
  return chat.status === 'idle' ? 'running' : 'done'
}

function delegatedSummary(chat: ChatView | undefined) {
  if (!chat || (chat.lifecycle && chat.lifecycle !== 'active'))
    return 'Conversation deleted'
  const state = delegatedState(chat)
  if (state === 'running' && chat.pendingQuestions > 0)
    return chat.pendingQuestions > 1 ? `${chat.pendingQuestions} questions need an answer` : 'Asks a question'
  if (state === 'running')
    return chat.status === 'queued' || chat.status === 'idle' ? 'Waiting for a runner' : chat.wakeup ? 'Waiting' : 'Working'
  if (state === 'failed')
    return 'Failed'
  if (state === 'stopped')
    return 'Stopped'
  return chat.preview || 'Reported back'
}

// A subagent still marked as working once the run has ended will not report anymore.
export function subagentState(agent: ActivityArtifact, active: boolean, chats?: Map<string, ChatView>): SubagentState {
  const delegation = agent.subagent?.delegation
  if (delegation)
    return delegatedState(chats?.get(delegation.chatId))
  const state = agent.subagent?.state ?? 'running'
  return state === 'running' && !active ? 'stopped' : state
}

export function subagentInitials(name: string) {
  const words = name.split(/\s+/).filter(word => /\w/.test(word))
  const letters = words.length > 1 ? words[0]![0]! + words[1]![0]! : name.replace(/\W/g, '').slice(0, 2)
  return letters.toUpperCase() || '·'
}

/** The same colour for a subagent in the conversation, the team bar and the panel. */
export const subagentColor = (agent: ActivityArtifact) => identityColor(agent.id)

function firstLine(value: string) {
  return value.split('\n').map(line => line.replace(/^[#>*\s-]+/, '').trim()).find(Boolean) ?? ''
}

/** What the subagent does now, or what it reported. */
export function subagentSummary(agent: ActivityArtifact, active: boolean, chats?: Map<string, ChatView>) {
  const subagent = agent.subagent!
  if (subagent.delegation)
    return delegatedSummary(chats?.get(subagent.delegation.chatId))
  const state = subagentState(agent, active)
  if (state === 'running') {
    const latest = subagent.steps.findLast(step => step.kind !== 'notice')
    const step = latest && [latest.title, latest.command || latest.subtitle].filter(Boolean).join(' · ')
    return subagent.activity || step || 'Starting…'
  }

  const report = firstLine(subagent.result) || firstLine(subagent.message)
  if (state === 'failed')
    return report || 'Failed without a report'
  if (state === 'stopped')
    return report || 'Stopped before reporting back'
  return report || 'Finished'
}

export function subagentElapsed(agent: ActivityArtifact, active: boolean, now: number) {
  // A delegated task's time is its conversation's.
  if (agent.subagent?.delegation)
    return ''
  if (subagentState(agent, active) === 'running')
    return liveElapsed(agent.time, now)
  // liveElapsed formats any span; anchor this one at an arbitrary positive start.
  return agent.durationMs === undefined ? '' : liveElapsed(1, 1 + agent.durationMs)
}

export function subagentDetails(subagent: ActivitySubagent) {
  return [
    subagent.model,
    subagent.toolUses === undefined ? '' : subagent.toolUses === 1 ? '1 tool use' : `${subagent.toolUses.toLocaleString()} tool uses`,
    subagent.tokens === undefined ? '' : `${subagent.tokens.toLocaleString()} tokens`,
  ].filter(Boolean).join(' · ')
}

/** Teams include nested delegations so every visible subagent can open its own panel. */
export function subagentTeams(entries: { kind: string }[]): ActivityArtifact[][] {
  const teams: ActivityArtifact[][] = []
  for (const entry of entries) {
    if (entry.kind !== 'team')
      continue
    const agents = (entry as Extract<ActivityEntry, { kind: 'team' }>).agents
    teams.push(agents)
    for (const agent of agents) {
      const steps = teamEntries([{ kind: 'group', id: agent.id, artifacts: agent.subagent?.steps ?? [] }])
      teams.push(...subagentTeams(steps))
    }
  }

  return teams
}

/** Current-request agents and any earlier children still working in the same execution. */
export function currentSubagents(entries: { kind: string }[]): ActivityArtifact[] {
  const start = entries.findLastIndex(entry => entry.kind === 'message' && (entry as Extract<ActivityEntry, { kind: 'message' }>).role === 'user')
  const current = new Set(subagentTeams(entries.slice(start + 1)).flat().map(agent => agent.id))
  // A delegated task reports through its own conversation: earlier ones stay with their request.
  return subagentTeams(entries).flat().filter(agent => current.has(agent.id) || (agent.subagent?.state === 'running' && !agent.subagent.delegation))
}

/** The bar replaces the explicit background wait only if it represents every awaited task. */
export function waitingForSubagents(events: RunEvent[], agents: ActivityArtifact[]) {
  const announcement = events.findLast(event => event.type === 'turn.waiting')
  const tasks = Array.isArray(announcement?.payload?.tasks) ? announcement.payload.tasks : []
  return tasks.length > 0 && tasks.every((task) => {
    const id = task && typeof task === 'object' ? (task as Record<string, unknown>).id : undefined
    return typeof id === 'string' && agents.some(agent => agent.subagent?.taskId === id)
  })
}
