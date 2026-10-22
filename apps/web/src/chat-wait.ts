import type { ChatView, ChatWakeup } from '../../../packages/contracts/chats'
import type { ActivityWait } from './activity'

/** What a conversation waits for: a timer, a plugin's condition or another Cairn run. */
export type WaitKind = 'timer' | 'condition' | 'run'
export type WaitTone = 'ok' | 'error' | 'warning' | 'neutral'

export interface WaitTarget {
  kind: WaitKind
  /** A sentence for the conversation. */
  title: string
  /** A few words for the Fil and the composer. */
  short: string
  /** Identifiers worth showing beside it, such as the commit. */
  details: string[]
}

export interface WaitOutcome {
  tone: WaitTone
  title: string
  detail: string
}

const words = (value: string) => value.replaceAll('_', ' ')

/** The time of day, with the date when it is not today. */
export function wakeTime(at: number, now = Date.now()) {
  const date = new Date(at)
  const today = date.toDateString() === new Date(now).toDateString()
  return date.toLocaleString([], today
    ? { hour: '2-digit', minute: '2-digit' }
    : {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      })
}

/** Time left before a deadline, rounded up so it never reads zero early. */
export function timeLeft(ms: number) {
  const seconds = Math.ceil(ms / 1000)
  if (seconds <= 0)
    return 'any moment'
  if (seconds < 60)
    return `${seconds} s`
  const minutes = Math.ceil(seconds / 60)
  if (minutes < 60)
    return `${minutes} min`
  const hours = Math.floor(minutes / 60)
  if (hours < 24)
    return minutes % 60 ? `${hours} h ${String(minutes % 60).padStart(2, '0')}` : `${hours} h`
  return `${Math.round(hours / 24)} d`
}

/** How long a finished wait lasted. */
export function waited(ms: number) {
  const minutes = Math.round(ms / 60000)
  return minutes < 1 ? 'under a minute' : timeLeft(minutes * 60000)
}

export function waitTarget(wakeup: Pick<ChatWakeup, 'condition' | 'display' | 'wakeAt'>, now = Date.now()): WaitTarget {
  const condition = wakeup.condition
  if (condition?.type === 'run_finished') {
    return {
      kind: 'run',
      title: 'Waiting for another run to finish',
      short: 'Another run',
      details: [],
    }
  }

  // A plugin condition reads as its plugin words it.
  if (condition) {
    const display = wakeup.display
    return {
      kind: 'condition',
      title: display?.title || 'Waiting for a condition',
      short: display?.short || 'Condition',
      details: display?.details ?? [],
    }
  }

  const at = `Resumes at ${wakeTime(wakeup.wakeAt, now)}`
  return {
    kind: 'timer',
    title: at,
    short: at,
    details: [],
  }
}

/**
 * A wait seen only in a run's history: whether it still stands is unknown, so it reads as what
 * was scheduled rather than what will happen.
 */
export function recordedTitle(wakeup: Pick<ChatWakeup, 'condition' | 'display' | 'wakeAt'>, now = Date.now()) {
  const target = waitTarget(wakeup, now)
  return target.kind === 'timer' ? `Scheduled to resume at ${wakeTime(wakeup.wakeAt, now)}` : `Wait scheduled · ${target.short}`
}

/**
 * The live status line: a timer resumes at its deadline, a condition at the latest by it.
 * An overdue wait resumes when capacity allows.
 */
export function waitCountdown(wakeup: Pick<ChatWakeup, 'condition' | 'wakeAt'>, now = Date.now()) {
  const left = wakeup.wakeAt - now
  if (left <= 0)
    return 'Resuming soon'
  return wakeup.condition ? `up to ${timeLeft(left)}` : `in ${timeLeft(left)}`
}

/**
 * The wakeup a conversation still honours: a failed or interrupted turn never resumes it, even
 * when the record lingers. A paused conversation keeps it.
 */
export function liveWakeup(chat: (Pick<ChatView, 'wakeup' | 'lifecycle' | 'status'> & { run?: { status: string } | null }) | null | undefined) {
  const status = chat?.run?.status ?? chat?.status
  if (!chat?.wakeup || (chat.lifecycle && chat.lifecycle !== 'active') || ['failed', 'interrupted', 'cancelled'].includes(status ?? ''))
    return null
  return chat.wakeup
}

/** Its result already arrived, or its deadline passed: it resumes as soon as capacity allows. */
export function waitDue(wakeup: Pick<ChatWakeup, 'observation' | 'wakeAt'>, now = Date.now()) {
  return !!wakeup.observation || wakeup.wakeAt <= now
}

/** The conversation header while it waits. */
export function waitStatus(wakeup: Pick<ChatWakeup, 'condition' | 'observation' | 'wakeAt'>, now = Date.now()) {
  if (waitDue(wakeup, now))
    return 'Resuming soon'
  return wakeup.condition ? `Awaiting result · up to ${timeLeft(wakeup.wakeAt - now)}` : `Resumes in ${timeLeft(wakeup.wakeAt - now)}`
}

/** The conversation's pending wakeup, as the conversation page knows it. */
export interface LiveWait {
  wakeup: ChatWakeup | null
  paused: boolean
  /** The turn that scheduled it ended successfully: only then does Cairn check its condition. */
  settled: boolean
  /** Another action is under way, so pausing would be ignored. */
  busy?: boolean
  /** Mission waits can be cancelled with the run, but have no conversation pause. */
  canPause?: boolean
}

/** Where a wait card stands, from its history and, when known, the conversation's live state. */
export interface WaitProgress {
  /** Its wakeup is the one the conversation still honours. */
  current: boolean
  /** What Cairn observed, from the history or the pending wakeup; `undefined` before any result. */
  observation: Record<string, unknown> | null | undefined
  /** Its result arrived or the conversation resumed. */
  finished: boolean
  replaced: boolean
  cancelled: boolean
  paused: boolean
  /** It still counts down. */
  pending: boolean
  /** Once its turn ended, the conversation can be paused before it resumes, even with the result in. */
  pausable: boolean
}

export function waitProgress(wait: ActivityWait, live?: LiveWait): WaitProgress {
  const current = !!wait.wakeup && live?.wakeup?.id === wait.wakeup.id
  // Plugin condition polling records the result on the pending wakeup too, which matters when its event is
  // unloaded. Another run's result is only computed when the conversation resumes.
  const observation = wait.observation !== undefined ? wait.observation : current ? (live!.wakeup!.observation ?? undefined) : undefined
  const finished = !!wait.resumedAt || observation !== undefined
  const replaced = wait.ended === 'replaced'
  // Live state, when known, is authoritative: stopping a wait leaves no event in the history.
  const cancelled = live ? !current && !replaced : wait.ended === 'cancelled'
  const paused = current && live!.paused
  return {
    current,
    observation,
    finished,
    replaced,
    cancelled,
    paused,
    pending: current && !paused && !finished,
    pausable: current && !paused && !wait.resumedAt && live!.settled && live!.canPause !== false,
  }
}

/** Time left in the Fil, which refreshes every 30 s: seconds would read stale. */
export function waitLeft(wakeup: { wakeAt: number, condition?: boolean, resolved?: boolean }, now = Date.now()) {
  const left = wakeup.wakeAt - now
  if (wakeup.resolved || left <= 0)
    return 'soon'
  if (left < 60000)
    return '< 1 min'
  return wakeup.condition ? `≤ ${timeLeft(left)}` : timeLeft(left)
}

/** A plugin's words for a result, when it gave them. */
function resultDisplay(observation: Record<string, unknown>) {
  const display = observation.display as Partial<WaitOutcome> | null | undefined
  if (!display || typeof display !== 'object' || typeof display.title !== 'string' || !display.title)
    return null
  const tone = ['ok', 'error', 'warning', 'neutral'].includes(display.tone as string) ? display.tone as WaitTone : 'neutral'
  return { tone, title: display.title, detail: typeof display.detail === 'string' ? display.detail : '' }
}

/**
 * What a result answers, for a card that has no scheduled wakeup to name it. A plugin result
 * only names itself when its plugin gives a short wording.
 */
export function observedTarget(observation: Record<string, unknown> | null | undefined) {
  if (!observation)
    return ''
  if (observation.type === 'run_finished')
    return 'Another run'
  if (observation.type === 'time')
    return 'Scheduled time'
  const display = observation.display as { short?: unknown } | null | undefined
  if (typeof display?.short === 'string' && display.short)
    return display.short
  return observation.type || observation.pluginId ? 'A condition' : ''
}

const runOutcomes: Record<string, string> = { blocked: 'Blocked', needs_input: 'Needs your input' }
const failures = ['failure', 'failed', 'error', 'startup_failure', 'timed_out']

/** What Cairn observed when a wait ended; time reached is distinct from an external timeout. */
export function waitOutcome(observation: Record<string, unknown> | null | undefined): WaitOutcome {
  const state = observation?.state
  if (!observation || !state)
    return { tone: 'neutral', title: 'Wait finished', detail: '' }

  const display = resultDisplay(observation)
  if (display)
    return display
  if (state === 'completed' && observation.type === 'time')
    return { tone: 'neutral', title: 'Scheduled time reached', detail: '' }
  if (state === 'timed_out')
    return { tone: 'warning', title: 'Deadline reached without a result', detail: '' }
  if (state === 'unavailable')
    return { tone: 'warning', title: 'Result unavailable', detail: typeof observation.message === 'string' ? observation.message : '' }

  if (observation.type === 'run_finished') {
    const status = String(observation.status ?? '')
    if (status === 'succeeded')
      return { tone: 'ok', title: 'The run finished', detail: runOutcomes[String(observation.outcomeStatus)] ?? '' }
    const ended = status === 'failed' ? 'failed' : status === 'cancelled' ? 'was cancelled' : 'was interrupted'
    return { tone: status === 'failed' ? 'error' : 'warning', title: `The run ${ended}`, detail: '' }
  }

  // A plugin result without its own wording: its conclusion, when it has one.
  const conclusion = observation.conclusion
  if (typeof conclusion === 'string' && conclusion) {
    if (conclusion === 'success')
      return { tone: 'ok', title: 'Succeeded', detail: '' }
    const title = words(conclusion)
    return { tone: failures.includes(conclusion) ? 'error' : 'warning', title: title[0]!.toUpperCase() + title.slice(1), detail: '' }
  }

  return { tone: 'neutral', title: 'Wait finished', detail: '' }
}
