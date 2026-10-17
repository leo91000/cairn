import type { RunEvent } from '../../../packages/contracts/contracts'
import type { ActivityEntry } from '../src/activity'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import {
  continuesPlan,
  currentPlan,
  currentStep,
  planChange,
  planProgress,
  planSteps,
} from '../src/agent-plan'

let id = 0

function event(type: string, payload?: Record<string, unknown>, text = ''): RunEvent {
  id++
  return {
    id,
    runId: 'chat',
    createdAt: id * 1000,
    type,
    text,
    ...(payload ? { payload } : {}),
  }
}

function plan(itemId: string, steps: [string, string, string?][], fields: Record<string, unknown> = {}) {
  return event('item.updated', {
    item: {
      id: itemId,
      type: 'todo_list',
      items: steps.map(([text, status, active_text]) => ({
        text,
        status,
        completed: status === 'completed',
        ...(active_text ? { active_text } : {}),
      })),
      ...fields,
    },
  })
}

function command(command: string) {
  return event('item.completed', {
    item: {
      id: `cmd-${id}`,
      type: 'command_execution',
      command,
      exit_code: 0,
      status: 'completed',
    },
  })
}

const user = (text: string) => event('chat.user', { text })
const kinds = (entries: ActivityEntry[]) => entries.map(entry => entry.kind)
const updates = (entries: ActivityEntry[]) => entries.filter(entry => entry.kind === 'plan-update')

describe('agent plan', () => {
  it('reads step states, and only completion from agents that report nothing else', () => {
    const steps = planSteps({
      items: [
        { text: 'Explore', status: 'completed', completed: true },
        {
          text: 'Migrate',
          status: 'in_progress',
          completed: false,
          active_text: 'Migrating',
        },
        { text: 'Document', status: 'cancelled', completed: false },
        { text: 'Check', completed: false },
        { text: 'Legacy done', completed: true },
        { text: '  ' },
      ],
    })

    expect(steps.map(step => step.state)).toEqual(['done', 'active', 'cancelled', 'pending', 'done'])
    expect(steps[1]!.activeText).toBe('Migrating')
    // Dropped steps leave the plan's total.
    expect(planProgress(steps)).toEqual({ done: 2, total: 4 })
    expect(currentStep(steps)).toMatchObject({ index: 1, active: true })
    expect(currentStep(steps.map(step => step.state === 'active' ? { ...step, state: 'pending' as const } : step))).toMatchObject({ index: 1, active: false })
  })

  it('names what an update finished, started, added and dropped', () => {
    const before = planSteps({ items: [{ text: 'A', status: 'in_progress' }, { text: 'B', status: 'pending' }, { text: 'C', status: 'pending' }] })
    const after = planSteps({ items: [{ text: 'A', status: 'completed' }, { text: 'B', status: 'in_progress' }, { text: 'C', status: 'cancelled' }, { text: 'D', status: 'pending' }] })

    expect(planChange(before, after)).toEqual({
      done: ['A'],
      started: ['B'],
      added: ['D'],
      cancelled: ['C'],
    })
    expect(planChange(after, after)).toBeNull()
    // Claude Code drops a step by leaving it out; a finished step that leaves is not news.
    const trimmed = planSteps({ items: [{ text: 'A', status: 'completed' }, { text: 'D', status: 'in_progress' }] })
    expect(planChange(after, trimmed)).toEqual({
      done: [],
      started: ['D'],
      added: [],
      cancelled: ['B'],
    })
    expect(continuesPlan(before, after)).toBe(true)
    expect(continuesPlan(before, planSteps({ items: [{ text: 'Unrelated', status: 'pending' }] }))).toBe(false)
  })

  it('shows the plan once where the agent wrote it, then each change where it happened', () => {
    const entries = activityEntries([
      user('Add session expiry'),
      plan('plan:turn', [['Explore', 'in_progress'], ['Migrate', 'pending']], { explanation: 'Data model first.' }),
      command('rg -n session src'),
      plan('plan:turn', [['Explore', 'completed'], ['Migrate', 'in_progress'], ['Check', 'pending']]),
      // Claude Code repeats the list when its call completes: nothing changed.
      plan('plan:turn', [['Explore', 'completed'], ['Migrate', 'in_progress'], ['Check', 'pending']]),
      command('cargo test'),
      plan('plan:turn', [['Explore', 'completed'], ['Migrate', 'completed'], ['Check', 'completed']]),
    ], true)

    expect(kinds(entries)).toEqual(['message', 'plan', 'group', 'plan-update', 'group', 'plan-update'])
    const card = entries[1] as Extract<ActivityEntry, { kind: 'plan' }>
    // The card keeps the latest version of the plan and its explanation.
    expect(card.plan.steps.map(step => step.state)).toEqual(['done', 'done', 'done'])
    expect(card.plan.explanation).toBe('Data model first.')
    expect(updates(entries).map(entry => [entry.planId, entry.change, entry.progress])).toEqual([
      [card.id, {
        done: ['Explore'],
        started: ['Migrate'],
        added: ['Check'],
        cancelled: [],
      }, { done: 1, total: 3 }],
      [card.id, {
        done: ['Migrate', 'Check'],
        started: [],
        added: [],
        cancelled: [],
      }, { done: 3, total: 3 }],
    ])
    expect(JSON.stringify(entries)).not.toContain('Updated the plan')
  })

  it('keeps one card when an agent repeats the list that finished the plan', () => {
    const entries = activityEntries([
      plan('todo-1', [['Explore', 'in_progress']]),
      plan('todo-2', [['Explore', 'completed']]),
      // Claude Code repeats the list when the call completes, once the plan is already finished.
      plan('todo-2', [['Explore', 'completed']]),
    ], true)

    expect(kinds(entries)).toEqual(['plan', 'plan-update'])
  })

  it('starts a new plan once the previous one is finished or replaced', () => {
    const entries = activityEntries([
      plan('first', [['Explore', 'completed']]),
      plan('second', [['Explore', 'in_progress']]),
      plan('third', [['Something else', 'pending']]),
    ], true)

    expect(kinds(entries)).toEqual(['plan', 'plan', 'plan'])
  })

  it('leaves empty and failed updates out, and keeps a subagent plan among its steps', () => {
    const spawn = event('item.started', {
      item: {
        id: 'agent',
        type: 'subagent',
        name: 'Review',
        status: 'in_progress',
      },
    })
    const entries = activityEntries([
      plan('announced', []),
      plan('failed', [['Explore', 'pending']], { status: 'failed' }),
      spawn,
      plan('child-plan', [['Review the keyboard', 'in_progress']], { parent_id: 'agent' }),
    ], true)

    expect(kinds(entries)).toEqual(['team'])
    const team = entries[0] as Extract<ActivityEntry, { kind: 'team' }>
    expect(team.agents[0]!.subagent!.steps.map(step => [step.kind, step.tasks])).toEqual([
      ['plan', [{ text: 'Review the keyboard', completed: false }]],
    ])
  })

  it('keeps the current request plan beside the composer until it is finished', () => {
    const working = [
      user('Add session expiry'),
      plan('plan', [['Explore', 'completed'], ['Migrate', 'in_progress', 'Adding the migration']]),
    ]
    expect(currentPlan(activityEntries(working, true))?.steps.map(step => step.text)).toEqual(['Explore', 'Migrate'])

    // A new message leaves the earlier plan behind until the agent changes it again.
    const followUp = [...working, user('Also update the docs')]
    expect(currentPlan(activityEntries(followUp, true))).toBeUndefined()
    const resumed = [...followUp, plan('plan', [['Explore', 'completed'], ['Migrate', 'completed'], ['Docs', 'in_progress']])]
    expect(currentPlan(activityEntries(resumed, true))?.steps.map(step => step.state)).toEqual(['done', 'done', 'active'])
    // The new request gets its own card instead of rewriting the earlier one further up.
    expect(kinds(activityEntries(resumed, true))).toEqual(['message', 'plan', 'message', 'plan'])

    const finished = [...working, plan('plan', [['Explore', 'completed'], ['Migrate', 'completed']])]
    expect(currentPlan(activityEntries(finished, true))).toBeUndefined()
  })
})
