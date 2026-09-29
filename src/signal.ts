import type { ChatView } from '../shared/chats'
import type { RunListItem, Task } from '../shared/contracts'
import type { ActivityEntry } from './activity'

// Same identity palette as the Android « Signal » design: readable on white text in both themes.
const identityColors = ['#4545EF', '#D9542F', '#B23F8C', '#12806F', '#8A5A12', '#3F7FBF', '#7A4FD1', '#5E6B2E']

/** A deterministic colour per agent or project, matching Android's `String.hashCode`. */
export function identityColor(key: string) {
  let hash = 0
  for (const char of key)
    hash = (Math.imul(hash, 31) + char.charCodeAt(0)) | 0
  return identityColors[((hash % identityColors.length) + identityColors.length) % identityColors.length]
}

export const initial = (name: string) => name.trim().match(/[\p{L}\p{N}]/u)?.[0]?.toUpperCase() ?? '?'

export type FilKind = 'question' | 'failed' | 'failed-mission' | 'review' | 'running' | 'queued' | 'paused' | 'recent'
export interface FilItem {
  key: string
  kind: FilKind
  title: string
  subtitle: string
  agent: string
  agentKey: string
  project: string | null
  at: number
  to: string
  chatId?: string
  taskId?: string
}
export interface Fil { forYou: FilItem[], live: FilItem[], recent: FilItem[] }

const outcomeLabels = { blocked: 'Blocked', needs_input: 'Needs your input' } as const

/**
 * What needs the user first (questions, failures, missions to review), then live work,
 * then recent conversations. Only the latest run of each mission counts, and chat runs
 * belong to their conversation.
 */
export function filOf(chats: ChatView[], tasks: Task[], runs: RunListItem[]): Fil {
  const forYou: FilItem[] = []
  const live: FilItem[] = []
  const recent: FilItem[] = []
  for (const chat of chats) {
    if (chat.lifecycle && chat.lifecycle !== 'active')
      continue
    const base = {
      key: `chat:${chat.id}`,
      title: chat.title || 'New conversation',
      agent: chat.agentName,
      agentKey: chat.agentId,
      project: chat.projectName,
      at: chat.updatedAt,
      to: `/chats/${chat.id}`,
      chatId: chat.id,
    }
    if (chat.pendingQuestions > 0)
      forYou.push({ ...base, kind: 'question', subtitle: chat.pendingQuestions > 1 ? `${chat.pendingQuestions} questions need your answer` : 'Needs your answer' })
    else if (chat.status === 'failed' || chat.status === 'interrupted')
      forYou.push({ ...base, kind: 'failed', subtitle: chat.status === 'failed' ? 'Failed' : 'Interrupted' })
    else if (chat.paused)
      recent.push({ ...base, kind: 'paused', subtitle: 'Paused' })
    else if (chat.status === 'running')
      live.push({ ...base, kind: 'running', subtitle: 'Working' })
    else if (chat.status === 'queued')
      live.push({ ...base, kind: 'queued', subtitle: 'Waiting for a runner' })
    else
      recent.push({ ...base, kind: 'recent', subtitle: [chat.agentName, chat.projectName].filter(Boolean).join(' · ') })
  }

  const latest = new Map<string, RunListItem>()
  for (const run of runs) {
    if (run.trigger === 'chat')
      continue
    const current = latest.get(run.taskId)
    if (!current || run.createdAt > current.createdAt)
      latest.set(run.taskId, run)
  }

  for (const task of tasks) {
    const run = latest.get(task.id)
    if (task.archived || !run)
      continue
    const base = {
      key: `task:${task.id}`,
      title: task.name,
      agent: run.agentName,
      agentKey: task.agentId,
      project: null,
      at: run.finishedAt ?? run.startedAt ?? run.createdAt,
      to: `/runs/${run.id}`,
      taskId: task.id,
    }
    if (run.status === 'running' || run.status === 'queued')
      live.push({ ...base, kind: run.status === 'running' ? 'running' : 'queued', subtitle: run.status === 'running' ? 'Mission running' : 'Mission waiting for a runner' })
    else if (run.status === 'failed' || run.status === 'interrupted')
      forYou.push({ ...base, kind: 'failed-mission', subtitle: run.error?.split('\n')[0] || (run.status === 'failed' ? 'Mission failed' : 'Mission interrupted') })
    else if (run.status === 'succeeded' && run.outcome && run.outcome.status !== 'completed')
      forYou.push({ ...base, kind: 'review', subtitle: `${outcomeLabels[run.outcome.status]} · ${run.outcome.reason}` })
  }

  const newest = (a: FilItem, b: FilItem) => b.at - a.at
  return { forYou: forYou.sort(newest), live: live.sort(newest), recent: recent.sort(newest) }
}

/** What the working indicator says: the step in progress, or the last one reached. */
export interface WorkingStep { title: string, detail: string, since: number | null }

/**
 * The agent's current step since the latest user message: an action in progress is named
 * with its command; otherwise the agent is simply working, with its last completed step.
 */
export function workingStep(entries: ActivityEntry[], agent: string, fallbackStart: number | null): WorkingStep {
  const turnStart = entries.findLastIndex(entry => entry.kind === 'message' && entry.role === 'user')
  const turn = turnStart >= 0 ? entries.slice(turnStart + 1) : entries
  const opening = entries[turnStart]
  const since = (opening?.kind === 'message' && opening.time > 0 ? opening.time : null) ?? fallbackStart
  const artifacts = turn.flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])
  const latest = artifacts.findLast(artifact => artifact.kind !== 'notice')
  const name = agent.trim() || 'The agent'
  if (!latest)
    return { title: `${name} is working`, detail: '', since }
  if (latest.status === 'running')
    return { title: latest.title, detail: latest.command || latest.subtitle, since }
  return { title: `${name} is working`, detail: `Last step: ${latest.title}`, since }
}

/** Seconds precision for the live indicator: "45s", "2m 04s", "1h 05". */
export function liveElapsed(start: number | null, now = Date.now()) {
  if (!start || start <= 0)
    return ''
  const seconds = Math.floor(Math.max(0, now - start) / 1000)
  if (seconds < 60)
    return `${seconds}s`
  if (seconds < 3600)
    return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`
  return `${Math.floor(seconds / 3600)}h ${String(Math.floor((seconds % 3600) / 60)).padStart(2, '0')}`
}

/** Compact age for lists: "now", "4m", "3h", "2d", then the date. */
export function shortAge(at: number, now = Date.now()) {
  const seconds = Math.max(0, Math.round((now - at) / 1000))
  if (seconds < 45)
    return 'now'
  if (seconds < 3600)
    return `${Math.round(seconds / 60)}m`
  if (seconds < 86400)
    return `${Math.round(seconds / 3600)}h`
  if (seconds < 7 * 86400)
    return `${Math.round(seconds / 86400)}d`
  return new Date(at).toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

export function greeting(date = new Date()) {
  const hour = date.getHours()
  return hour < 5 ? 'Good night' : hour < 12 ? 'Good morning' : hour < 18 ? 'Good afternoon' : 'Good evening'
}
