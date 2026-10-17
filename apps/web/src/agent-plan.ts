import type { ActivityEntry } from './activity'

/**
 * The plan an agent keeps while it works. Codex, Claude Code and OpenCode each report the whole
 * list at every update, as one `todo_list` item. Mirrors Android's AgentPlan.kt.
 */
export type PlanStepState = 'pending' | 'active' | 'done' | 'cancelled'

export interface PlanStep {
  text: string
  /** What the agent does during the step, when it says so (Claude Code's `activeForm`). */
  activeText: string
  state: PlanStepState
}

export interface AgentPlan {
  steps: PlanStep[]
  /** Why the agent chose this plan (Codex). */
  explanation: string
  /** When the agent wrote the plan, then last changed it. */
  time: number
  updatedAt: number
}

/** Steps done out of the steps still part of the plan. */
export interface PlanProgress {
  done: number
  total: number
}

/** What one update changed in the plan, named by the steps' text. */
export interface PlanChange {
  done: string[]
  started: string[]
  added: string[]
  cancelled: string[]
}

const states: Record<string, PlanStepState> = {
  completed: 'done',
  in_progress: 'active',
  inProgress: 'active',
  cancelled: 'cancelled',
  pending: 'pending',
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {}
}

function text(value: unknown) {
  return typeof value === 'string' ? value : ''
}

/** Steps of a `todo_list` item. Older items, and Codex exec, only say whether a step is done. */
export function planSteps(item: Record<string, unknown>): PlanStep[] {
  return (Array.isArray(item.items) ? item.items : [])
    .map(record)
    .map(step => ({
      text: text(step.text).trim(),
      activeText: text(step.active_text).trim(),
      state: states[text(step.status)] ?? (step.completed === true ? 'done' : 'pending'),
    }))
    .filter(step => step.text)
}

/** Every step is done or abandoned. */
export function planFinished(steps: PlanStep[]) {
  return steps.length > 0 && steps.every(step => step.state === 'done' || step.state === 'cancelled')
}

/** Steps done out of the steps still part of the plan. */
export function planProgress(steps: PlanStep[]): PlanProgress {
  const kept = steps.filter(step => step.state !== 'cancelled')
  return { done: kept.filter(step => step.state === 'done').length, total: kept.length }
}

/** The step in progress, or the next one when the agent does not mark progress. */
export function currentStep(steps: PlanStep[]) {
  const index = steps.findIndex(step => step.state === 'active')
  if (index >= 0)
    return { step: steps[index]!, index, active: true }
  const next = steps.findIndex(step => step.state === 'pending')
  return next >= 0 ? { step: steps[next]!, index: next, active: false } : undefined
}

/** The same steps in the same states: agents repeat the list when its call completes. */
export function samePlan(previous: PlanStep[], next: PlanStep[]) {
  return previous.length === next.length && previous.every((step, index) => step.text === next[index]!.text && step.state === next[index]!.state)
}

/** A plan still under way continues with any list sharing one of its steps. */
export function continuesPlan(previous: PlanStep[], next: PlanStep[]) {
  if (planFinished(previous))
    return false
  const known = new Set(previous.map(step => step.text))
  return next.some(step => known.has(step.text))
}

/**
 * What changed between two versions of a plan, or null when nothing visible did. Claude Code drops
 * a step by leaving it out of the list.
 */
export function planChange(previous: PlanStep[], next: PlanStep[]): PlanChange | null {
  const before = new Map(previous.map(step => [step.text, step.state]))
  const kept = new Set(next.map(step => step.text))
  const change: PlanChange = {
    done: [],
    started: [],
    added: [],
    cancelled: [],
  }
  for (const step of next) {
    const state = before.get(step.text)
    if (state === step.state)
      continue
    if (step.state === 'done')
      change.done.push(step.text)
    else if (step.state === 'cancelled')
      change.cancelled.push(step.text)
    else if (step.state === 'active')
      change.started.push(step.text)
    else if (state === undefined)
      change.added.push(step.text)
  }

  for (const step of previous) {
    if (!kept.has(step.text) && (step.state === 'pending' || step.state === 'active'))
      change.cancelled.push(step.text)
  }

  return Object.values(change).some(steps => steps.length) ? change : null
}

/** "Completed X, started Y", for assistive technologies. */
export function planChangeLabel(change: PlanChange) {
  const parts = [
    change.done.length ? `completed ${change.done.join(', ')}` : '',
    change.started.length ? `started ${change.started.join(', ')}` : '',
    change.added.length ? `added ${change.added.join(', ')}` : '',
    change.cancelled.length ? `dropped ${change.cancelled.join(', ')}` : '',
  ].filter(Boolean)
  return `Plan updated: ${parts.join('; ')}`
}

/**
 * The plan of the current request: the latest one the agent wrote or changed since your last
 * message, while some of it remains.
 */
export function currentPlan(entries: ActivityEntry[]): AgentPlan | undefined {
  const start = entries.findLastIndex(entry => entry.kind === 'message' && entry.role === 'user')
  const latest = entries.findLast(entry => entry.kind === 'plan' || entry.kind === 'plan-update')
  if (!latest || entries.indexOf(latest) < start)
    return undefined
  const id = latest.kind === 'plan' ? latest.id : latest.planId
  const card = entries.find((entry): entry is Extract<ActivityEntry, { kind: 'plan' }> => entry.kind === 'plan' && entry.id === id)
  return card && !planFinished(card.plan.steps) ? card.plan : undefined
}
