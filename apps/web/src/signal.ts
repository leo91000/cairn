import type { Provider } from '../../../packages/contracts/accounts'
import type { ChatView } from '../../../packages/contracts/chats'
import type { RunListItem, Task } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import type { ActivityEntry } from './activity'
import { isQuotaWait } from './chat-quota'
import { waitTarget } from './chat-wait'
import { exclusiveSuspensionLabel, isExclusivelySuspended } from './run-status'
import { chatPath } from './workspace-links'

// Same identity palette as the Android « Signal » design: readable on white text in both themes.
const identityColors = ['#14365A', '#D9542F', '#B23F8C', '#12806F', '#8A5A12', '#3F7FBF', '#7A4FD1', '#5E6B2E']

/** A deterministic colour per agent or project, matching Android's `String.hashCode`. */
export function identityColor(key: string) {
  let hash = 0
  for (const char of key)
    hash = (Math.imul(hash, 31) + char.charCodeAt(0)) | 0
  return identityColors[((hash % identityColors.length) + identityColors.length) % identityColors.length]
}

export const initial = (name: string) => name.trim().match(/[\p{L}\p{N}]/u)?.[0]?.toUpperCase() ?? '?'

export type FilKind = 'question' | 'failed' | 'failed-mission' | 'review' | 'unread' | 'running' | 'queued' | 'waiting' | 'snoozed' | 'set-aside' | 'paused' | 'suspended' | 'recent'

/**
 * Why a blocked mission no longer asks for the user: it waits on the agent work that started it,
 * on an outside system, or the user dismissed it.
 */
export type SetAside = 'requester' | 'external' | 'dismissed'
export interface FilItem {
  key: string
  kind: FilKind
  title: string
  subtitle: string
  agent: string
  agentKey: string
  /** The coding agent of its latest turn. */
  harness?: Provider
  project: string | null
  at: number
  to: string
  /** The opening of the agent's final message, for finished work not read yet. */
  preview?: string
  chatId?: string
  taskId?: string
  runId?: string
  /** A waiting conversation's deadline: when it resumes, or at the latest for a condition. */
  wakeAt?: number
  /** It waits for a condition, so it may resume before `wakeAt`. */
  condition?: boolean
  /** Its result already arrived: it resumes as soon as capacity allows. */
  resolved?: boolean
  /** A mission's outcome that is not complete: what its actions apply to. */
  outcome?: 'blocked' | 'needs_input'
  /** Why a blocked mission sits in Recent rather than For you. */
  setAside?: SetAside
  /** The conversation or mission that started this one, while it still exists. */
  requester?: { title: string, to: string }
}
export interface Fil {
  forYou: FilItem[]
  live: FilItem[]
  waiting: FilItem[]
  recent: FilItem[]
}

/** Missions open beside the Fil, as conversations do, rather than in the Atelier. */
export const filRunPath = (runId: string) => `/fil/runs/${runId}`

export const outcomeLabels = { blocked: 'Blocked', needs_input: 'Needs your input' } as const

/** Only a blocked outcome can be dismissed; input the agent asked for can only be snoozed. */
export const dismissible = (item: FilItem) => item.kind === 'review' && item.outcome === 'blocked'

/** 9:00 tomorrow, local time: when a snoozed mission asks again. */
export function tomorrowMorning(now = Date.now()) {
  const date = new Date(now)
  date.setDate(date.getDate() + 1)
  date.setHours(9, 0, 0, 0)
  return date.getTime()
}

/** Finished work waiting to be read, as opposed to what needs an answer or a decision. */
export const isUnread = (item: FilItem) => item.kind === 'unread'

/**
 * What needs the user first (questions, failures, missions to review, then finished work they
 * have not read), then live work, waiting conversations, then recent conversations. Only the latest run of each mission
 * counts, and chat runs belong to their conversation. `read` holds marks taken on this page
 * before the server's lists reflect them, by the finish time that was read.
 *
 * A blocked mission asks the user only when it waits on them: one waiting on the agent work that
 * started it or on an outside system, or one the user dismissed, sits in Recent. A snoozed one
 * waits until `now` reaches its time.
 */
export function filOf(chats: ChatView[], tasks: Task[], runs: RunListItem[], read: Record<string, number> = {}, now = Date.now()): Fil {
  const unreadNow = (key: string, finishedAt: number | null | undefined) => !(read[key] >= (finishedAt ?? 0))
  const forYou: FilItem[] = []
  const live: FilItem[] = []
  const waiting: FilItem[] = []
  const recent: FilItem[] = []
  for (const chat of chats) {
    if (chat.lifecycle && chat.lifecycle !== 'active')
      continue
    const base = {
      key: `chat:${chat.id}`,
      title: chat.title || 'New conversation',
      agent: chat.agentName,
      agentKey: chat.agentId,
      harness: chat.provider ?? undefined,
      project: chat.projectName,
      at: chat.updatedAt,
      to: chatPath(chat),
      chatId: chat.id,
      // A delegated conversation names the conversation that delegated it.
      requester: chat.requester ? { title: chat.requester.title || 'New conversation', to: chatPath({ id: chat.requester.chatId }) } : undefined,
    }
    if (chat.pendingQuestions > 0) {
      forYou.push({ ...base, kind: 'question', subtitle: chat.pendingQuestions > 1 ? `${chat.pendingQuestions} questions need your answer` : 'Needs your answer' })
    }
    else if (chat.status === 'failed' || chat.status === 'interrupted') {
      forYou.push({ ...base, kind: 'failed', subtitle: chat.status === 'failed' ? 'Failed' : 'Interrupted' })
    }
    else if (chat.paused) {
      recent.push({ ...base, kind: 'paused', subtitle: 'Paused' })
    }
    else if (isExclusivelySuspended(chat)) {
      live.push({ ...base, kind: 'suspended', subtitle: exclusiveSuspensionLabel })
    }
    else if (isQuotaWait(chat)) {
      waiting.push({ ...base, kind: 'waiting', subtitle: 'Waiting for quota' })
    }
    else if (chat.status === 'running') {
      live.push({ ...base, kind: 'running', subtitle: 'Working' })
    }
    else if (chat.status === 'queued') {
      live.push({ ...base, kind: 'queued', subtitle: 'Needs a free runner' })
    }
    else if (chat.wakeup && chat.status === 'succeeded') {
      waiting.push({
        ...base,
        kind: 'waiting',
        subtitle: waitTarget(chat.wakeup).short,
        wakeAt: chat.wakeup.wakeAt,
        condition: !!chat.wakeup.condition,
        // Recorded by plugin condition polling; another run's result is only computed at resumption.
        resolved: !!chat.wakeup.observation,
      })
    }
    else if (chat.unread && unreadNow(base.key, chat.finishedAt)) {
      forYou.push({
        ...base,
        kind: 'unread',
        at: chat.finishedAt ?? chat.updatedAt,
        subtitle: 'Finished',
        preview: chat.preview || undefined,
      })
    }
    else {
      recent.push({ ...base, kind: 'recent', subtitle: [chat.agentName, chat.projectName].filter(Boolean).join(' · ') })
    }
  }

  const latest = new Map<string, RunListItem>()
  for (const run of runs) {
    if (run.trigger === 'chat')
      continue
    const current = latest.get(run.taskId)
    if (!current || run.createdAt > current.createdAt)
      latest.set(run.taskId, run)
  }

  // The work that started a mission, while it is still there to act.
  function requesterOf(run: RunListItem): FilItem['requester'] {
    const { chatId, taskId } = run.requestedBy ?? {}
    const chat = chatId ? chats.find(chat => chat.id === chatId && (!chat.lifecycle || chat.lifecycle === 'active')) : undefined
    if (chat)
      return { title: chat.title || 'New conversation', to: chatPath(chat) }
    const mission = taskId ? tasks.find(task => task.id === taskId && !task.archived) : undefined
    const missionRun = mission && latest.get(mission.id)
    if (mission && missionRun)
      return { title: mission.name, to: filRunPath(missionRun.id) }
    return undefined
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
      harness: run.provider ?? undefined,
      project: null,
      at: run.finishedAt ?? run.startedAt ?? run.createdAt,
      to: filRunPath(run.id),
      taskId: task.id,
      runId: run.id,
    }
    if (isExclusivelySuspended(run)) {
      live.push({ ...base, kind: 'suspended', subtitle: exclusiveSuspensionLabel })
    }
    else if (isQuotaWait(run)) {
      waiting.push({ ...base, kind: 'waiting', subtitle: 'Waiting for quota' })
    }
    // A mission that scheduled its continuation resumes on its own, like a chat.
    else if (run.status === 'waiting') {
      waiting.push({
        ...base,
        kind: 'waiting',
        subtitle: run.wakeup?.reason || 'Mission waiting for continuation',
        wakeAt: run.wakeup?.wakeAt,
        condition: !!run.wakeup?.condition,
        resolved: !!run.wakeup?.observation,
      })
    }
    else if (run.status === 'running' || run.status === 'queued') {
      live.push({ ...base, kind: run.status === 'running' ? 'running' : 'queued', subtitle: run.status === 'running' ? 'Mission running' : 'Mission needs a free runner' })
    }
    else if (run.status === 'failed' || run.status === 'interrupted') {
      forYou.push({ ...base, kind: 'failed-mission', subtitle: run.error?.split('\n')[0] || (run.status === 'failed' ? 'Mission failed' : 'Mission interrupted') })
    }
    else if (run.status === 'succeeded' && run.outcome && run.outcome.status !== 'completed') {
      const outcome = run.outcome.status
      const reason = `${outcomeLabels[outcome]} · ${run.outcome.reason}`
      // A requester that is gone leaves the user to act.
      const requester = run.outcome.waitingOn === 'requester' ? requesterOf(run) : undefined
      const until = run.dismissal?.until
      if (requester) {
        recent.push({
          ...base,
          kind: 'set-aside',
          outcome,
          setAside: 'requester',
          requester,
          subtitle: `Waiting on ${requester.title}`,
        })
      }
      else if (run.outcome.waitingOn === 'external') {
        recent.push({
          ...base,
          kind: 'set-aside',
          outcome,
          setAside: 'external',
          subtitle: 'Waiting on an outside check',
        })
      }
      else if (run.dismissal && !until && outcome === 'blocked') {
        recent.push({
          ...base,
          kind: 'set-aside',
          outcome,
          setAside: 'dismissed',
          subtitle: 'Dismissed',
        })
      }
      else if (until && until > now) {
        waiting.push({
          ...base,
          kind: 'snoozed',
          outcome,
          subtitle: `Snoozed · ${reason}`,
          wakeAt: until,
        })
      }
      else {
        forYou.push({
          ...base,
          kind: 'review',
          outcome,
          subtitle: reason,
        })
      }
    }
    else if (run.status === 'succeeded' && run.unread && unreadNow(base.key, run.finishedAt)) {
      forYou.push({
        ...base,
        kind: 'unread',
        subtitle: 'Mission finished',
        preview: run.preview || undefined,
      })
    }
  }

  const newest = (a: FilItem, b: FilItem) => b.at - a.at
  // What asks for an answer or a decision stays above what only waits to be read.
  const urgency = (a: FilItem, b: FilItem) => Number(isUnread(a)) - Number(isUnread(b)) || newest(a, b)
  // A wait whose result arrived resumes first, before any deadline.
  const soonest = (a: FilItem, b: FilItem) => Number(!!b.resolved) - Number(!!a.resolved) || (a.wakeAt ?? Infinity) - (b.wakeAt ?? Infinity)
  return {
    forYou: forYou.sort(urgency),
    live: live.sort(newest),
    waiting: waiting.sort(soonest),
    recent: recent.sort(newest),
  }
}

/**
 * What the working indicator says: the step in progress, or the last one reached. `waiting`
 * lists the background tasks an idle agent waits for; `since` is then when it began waiting.
 */
export interface WorkingStep {
  title: string
  detail: string
  since: number | null
  waiting?: string[]
  /** The agent only waits for its subagents. */
  subagents?: boolean
}

/** The server's current background wait, presented with local fallback labels. */
export function backgroundWait(cards: TimelineEntry[]): { tasks: string[], taskIds: string[], since: number } | null {
  const card = cards.findLast(entry => entry.content.kind === 'notice'
    && entry.content.notice.code === 'backgroundTasks'
    && entry.content.notice.waitingSince !== undefined)
  if (!card || card.content.kind !== 'notice')
    return null
  const notice = card.content.notice
  const tasks = (notice.tasks ?? []).map(task => task.trim() || 'Background task')
  return tasks.length ? { tasks, taskIds: notice.taskIds ?? [], since: notice.waitingSince! } : null
}

/** The indicator for an agent that only waits for its background tasks. */
export function waitingStep(wait: { tasks: string[], since: number }): WorkingStep {
  const count = wait.tasks.length
  return {
    title: count === 1 ? 'Waiting for a background task' : `Waiting for ${count} background tasks`,
    detail: wait.tasks.join(' · '),
    since: wait.since,
    waiting: wait.tasks,
  }
}

/**
 * The agent's current step since the latest user message: an action in progress is named
 * with its command; otherwise the agent is simply working, with its last completed step.
 */
export function workingStep(entries: ActivityEntry[], agent: string, fallbackStart: number | null): WorkingStep {
  const turnStart = entries.findLastIndex(entry => entry.kind === 'message' && entry.role === 'user')
  const turn = turnStart >= 0 ? entries.slice(turnStart + 1) : entries
  const opening = entries[turnStart]
  const since = (opening?.kind === 'message' && opening.time > 0 ? opening.time : null) ?? fallbackStart
  const artifacts = turn.flatMap(entry => entry.kind === 'group' ? entry.artifacts : entry.kind === 'team' ? entry.agents : [])
  const latest = artifacts.findLast(artifact => artifact.kind !== 'notice')
  const name = agent.trim() || 'The agent'
  if (!latest)
    return { title: `${name} is working`, detail: '', since }
  const subagents = artifacts.filter(artifact => artifact.kind === 'subagent' && artifact.status === 'running')
  if (latest.kind === 'subagent' && subagents.length) {
    const title = subagents.length === 1 ? 'Waiting for a subagent' : `Waiting for ${subagents.length} subagents`
    return {
      title,
      detail: subagents.map(subagent => subagent.title).join(' · '),
      since,
      subagents: true,
    }
  }

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
