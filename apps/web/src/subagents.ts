import type { InjectionKey } from 'vue'
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

// A subagent still marked as working once the run has ended will not report anymore.
export function subagentState(agent: ActivityArtifact, active: boolean): SubagentState {
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
export function subagentSummary(agent: ActivityArtifact, active: boolean) {
  const subagent = agent.subagent!
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
  return subagentTeams(entries).flat().filter(agent => current.has(agent.id) || agent.subagent?.state === 'running')
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
