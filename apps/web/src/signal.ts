import type { Provider } from '../../../packages/contracts/accounts'
import type { ChatView } from '../../../packages/contracts/chats'
import type { TimelineEntry } from '../../../packages/contracts/live'
import type { ActivityEntry } from './activity'
import type { UsableAgents } from './agent-use'
import { canDirectChat } from './agent-use'
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

export type FilKind = 'question' | 'failed' | 'review' | 'unread' | 'running' | 'queued' | 'waiting' | 'snoozed' | 'paused' | 'suspended' | 'recent'

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
  /** Read state is independent of the execution/action kind. */
  unread?: boolean
  /** Observed question/turn timestamp to acknowledge. */
  readAt?: number
  /** Latest observed activity that a snooze is allowed to cover. */
  snoozeAt?: number
  /** The opening of the agent's final message, for finished work not read yet. */
  preview?: string
  chatId: string
  /** A waiting or snoozed conversation's deadline. */
  wakeAt?: number
  /** It waits for a condition, so it may resume before `wakeAt`. */
  condition?: boolean
  /** Its result already arrived: it resumes as soon as capacity allows. */
  resolved?: boolean
  /** A conversation's blocked outcome that needs review. */
  outcome?: 'blocked'
  /** The conversation that delegated this one, while it still exists. */
  requester?: {
    title: string
    to: string
    chatId: string
  }
  /** The hook rule (or schedule) that started this conversation, if any (ADR-0094). */
  originRule?: {
    ruleId: string
    ruleName: string
    schedule: boolean
  }
  /** It finished as expected: its row shows no status label, only a quiet mark. */
  finished?: boolean
}
export interface Fil {
  forYou: FilItem[]
  live: FilItem[]
  waiting: FilItem[]
  recent: FilItem[]
}

export const outcomeLabels = { blocked: 'Blocked' } as const

/** 9:00 tomorrow, local time: default snooze deadline. */
export function tomorrowMorning(now = Date.now()) {
  const date = new Date(now)
  date.setDate(date.getDate() + 1)
  date.setHours(9, 0, 0, 0)
  return date.getTime()
}

/** Finished work waiting to be read, as opposed to what needs an answer or a decision. */
export const isUnread = (item: FilItem) => item.unread ?? (item.kind === 'unread' || item.kind === 'review')

/**
 * What needs the user first (questions, failures, blocked conversations to review, then finished
 * work they have not read), then live work, waiting/snoozed conversations, then recent conversations.
 * `read` holds marks taken on this page before the server's lists reflect them.
 */
export function filOf(
  chats: ChatView[],
  read: Record<string, number> = {},
  now = Date.now(),
  usable: UsableAgents = null,
): Fil {
  const unreadNow = (key: string, finishedAt: number | null | undefined) => !(read[key] >= (finishedAt ?? 0))
  const forYou: FilItem[] = []
  const live: FilItem[] = []
  const waiting: FilItem[] = []
  const recent: FilItem[] = []
  for (const chat of chats) {
    if (chat.lifecycle && chat.lifecycle !== 'active')
      continue
    const snoozedUntil = chat.snoozedUntil
    const readTimestamp = chat.readAt ?? chat.finishedAt ?? chat.updatedAt
    const reminderDue = !!snoozedUntil && !!chat.unread && readTimestamp >= snoozedUntil
    const isSnoozed = !!snoozedUntil && snoozedUntil > now && !reminderDue
    const unread = !isSnoozed
      && !!chat.unread
      && unreadNow(`chat:${chat.id}`, readTimestamp)
      && (reminderDue || !chat.requestedBy || chat.pendingQuestions > 0 || chat.outcome?.status === 'blocked')
    const blocked = chat.status === 'succeeded' && chat.outcome?.status === 'blocked' ? chat.outcome : null
    const requester = chat.requester
      ? { title: chat.requester.title || 'New conversation', to: chatPath({ id: chat.requester.chatId }), chatId: chat.requester.chatId }
      : undefined
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
      readAt: readTimestamp,
      snoozeAt: Math.max(readTimestamp, chat.lastActivityAt ?? 0),
      unread,
      requester,
      originRule: chat.originRule ?? undefined,
    }
    if (isSnoozed) {
      waiting.push({
        ...base,
        kind: 'snoozed',
        subtitle: 'Snoozed',
        wakeAt: snoozedUntil,
      })
    }
    // A question only those with access to its agent may answer waits for them.
    else if (chat.pendingQuestions > 0 && !canDirectChat(usable, chat, chats)) {
      waiting.push({ ...base, kind: 'waiting', subtitle: 'Waiting for someone with access' })
    }
    else if (chat.pendingQuestions > 0) {
      forYou.push({ ...base, kind: 'question', subtitle: chat.pendingQuestions > 1 ? `${chat.pendingQuestions} questions need your answer` : 'Needs your answer' })
    }
    else if (chat.status === 'failed' || chat.status === 'interrupted' || (chat.paused && chat.error)) {
      forYou.push({ ...base, kind: 'failed', subtitle: chat.error || (chat.status === 'interrupted' ? 'Interrupted' : 'Failed') })
    }
    else if (blocked) {
      const item = {
        ...base,
        at: chat.finishedAt ?? chat.updatedAt,
        outcome: 'blocked' as const,
        subtitle: `Blocked · ${blocked.reason}`,
      }
      if (unread)
        forYou.push({ ...item, kind: 'review' })
      else
        recent.push({ ...item, kind: 'recent' })
    }
    else if (reminderDue && unread) {
      forYou.push({
        ...base,
        kind: 'unread',
        at: readTimestamp,
        subtitle: 'Snooze ended',
      })
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
    else if (base.unread) {
      forYou.push({
        ...base,
        kind: 'unread',
        at: chat.finishedAt ?? chat.updatedAt,
        subtitle: 'Finished',
        finished: true,
        preview: chat.preview || undefined,
      })
    }
    else {
      recent.push({
        ...base,
        kind: 'recent',
        finished: chat.status === 'succeeded',
        subtitle: chat.status === 'succeeded' ? 'Finished' : [chat.agentName, chat.projectName].filter(Boolean).join(' · '),
      })
    }
  }

  const newest = (a: FilItem, b: FilItem) => b.at - a.at
  // What asks for an answer or a decision stays above what only waits to be read.
  const urgency = (a: FilItem, b: FilItem) => Number(a.kind === 'unread') - Number(b.kind === 'unread') || newest(a, b)
  // A wait whose result arrived resumes first, before any deadline.
  const soonest = (a: FilItem, b: FilItem) => Number(!!b.resolved) - Number(!!a.resolved) || (a.wakeAt ?? Infinity) - (b.wakeAt ?? Infinity)
  return {
    forYou: forYou.sort(urgency),
    live: live.sort(newest),
    waiting: waiting.sort(soonest),
    recent: recent.sort(newest),
  }
}

/** Compact reception views: acknowledging a notification never changes its execution status. */
export function filViews(fil: Fil) {
  const all = [...fil.forYou, ...fil.live, ...fil.waiting, ...fil.recent].sort((a, b) => b.at - a.at)
  const ongoing = all.filter(item => ['question', 'running', 'queued', 'waiting', 'snoozed', 'suspended', 'review'].includes(item.kind))
  return { unread: all.filter(isUnread), ongoing, all }
}

/**
 * Rows in reading order, each delegated conversation right below the conversation that
 * started it when both are listed. One that started without its requester in the list stays
 * in place and names it instead.
 */
export function filThread(items: FilItem[]): { item: FilItem, nested: boolean }[] {
  const byChat = new Map(items.map(item => [item.chatId, item]))
  const childrenOf = new Map<FilItem, FilItem[]>()
  const roots: FilItem[] = []
  for (const item of items) {
    const parent = item.requester?.chatId ? byChat.get(item.requester.chatId) : undefined
    if (parent && parent !== item)
      childrenOf.set(parent, [...childrenOf.get(parent) ?? [], item])
    else
      roots.push(item)
  }

  const rows: { item: FilItem, nested: boolean }[] = []
  const placed = new Set<FilItem>()
  const place = (item: FilItem, nested: boolean) => {
    if (placed.has(item))
      return
    placed.add(item)
    rows.push({ item, nested })
    for (const child of childrenOf.get(item) ?? [])
      place(child, true)
  }

  roots.forEach(item => place(item, false))
  // A requester cycle has no root: keep those rows rather than losing them.
  items.forEach(item => place(item, false))
  return rows
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
