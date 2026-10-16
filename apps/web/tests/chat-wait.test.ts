import type { ChatView, ChatWakeup } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import {
  timeLeft,
  waitCountdown,
  waitOutcome,
  waitStatus,
  waitTarget,
} from '../src/chat-wait'
import { filOf } from '../src/signal'

const sha = '0b2ddce3a1b2c3d4e5f60718293a4b5c6d7e8f90'
const checks = {
  type: 'github_pr_checks',
  projectId: 'project',
  prNumber: 193,
  headSha: sha,
} as const

function wakeup(values: Partial<ChatWakeup> = {}): ChatWakeup {
  return {
    id: 'wake',
    messageId: 'first',
    reason: 'Merge once CI passes',
    seconds: 1800,
    createdAt: 0,
    wakeAt: 1_800_000,
    ...values,
  }
}

function chat(id: string, values: Partial<ChatView> = {}): ChatView {
  return {
    id,
    title: id,
    agentId: 'main',
    projectId: null,
    runId: null,
    paused: false,
    createdAt: 1,
    updatedAt: 1,
    pendingQuestions: 0,
    agentName: 'Cairn',
    projectName: null,
    status: 'succeeded',
    ...values,
  }
}

let id = 0

function event(type: string, payload: Record<string, unknown>): RunEvent {
  return {
    id: ++id,
    runId: 'chat',
    createdAt: id * 60_000,
    type,
    text: '',
    payload,
  }
}

function toolCall(tool: string, server: string, values: Record<string, unknown> = {}) {
  return event('item.completed', {
    item: {
      id: `tool-${id}`,
      type: 'mcp_tool_call',
      tool,
      server,
      status: 'completed',
      arguments: { reason: 'Merge once CI passes' },
      result: { content: [{ type: 'text', text: '{"deferred":true}' }] },
      ...values,
    },
  })
}

describe('scheduled waits', () => {
  it('names what the conversation waits for', () => {
    expect(waitTarget(wakeup({ condition: checks }))).toMatchObject({ kind: 'pr', short: 'PR #193 checks', details: ['0b2ddce'] })
    expect(waitTarget(wakeup({
      condition: {
        type: 'github_workflow',
        projectId: 'project',
        runId: 42,
        headSha: sha,
      },
    }))).toMatchObject({ kind: 'workflow', details: ['Run 42', '0b2ddce'] })
    expect(waitTarget(wakeup({ condition: { type: 'run_finished', runId: 'run' } })).kind).toBe('run')
    expect(waitTarget(wakeup()).short).toMatch(/^Resumes at /)
  })

  it('counts down to a timer, and to the deadline of a condition', () => {
    expect(timeLeft(45_000)).toBe('45 s')
    expect(timeLeft(61_000)).toBe('2 min')
    expect(timeLeft(65 * 60_000)).toBe('1 h 05')
    expect(waitCountdown(wakeup(), 600_000)).toBe('in 20 min')
    expect(waitCountdown(wakeup({ condition: checks }), 600_000)).toBe('up to 20 min')
    expect(waitCountdown(wakeup(), 2_000_000)).toBe('Resuming soon')
    expect(waitStatus(wakeup(), 600_000)).toBe('Resumes in 20 min')
    expect(waitStatus(wakeup({ condition: checks }), 600_000)).toBe('Waiting · up to 20 min')
    expect(waitStatus(wakeup(), 1_800_000)).toBe('Resuming soon')
    expect(waitStatus(wakeup({ condition: checks }), 2_000_000)).toBe('Resuming soon')
  })

  it('says how the wait ended', () => {
    expect(waitOutcome({
      state: 'completed',
      type: 'github_pr_checks',
      conclusion: 'success',
      requiredCount: 4,
    })).toEqual({ tone: 'ok', title: 'Required checks passed', detail: '4 checks' })
    expect(waitOutcome({
      state: 'completed',
      type: 'github_pr_checks',
      conclusion: 'failure',
      checks: [{ name: 'lint', state: 'failure' }, { name: 'test', state: 'success' }],
    })).toEqual({ tone: 'error', title: 'Required checks failed', detail: 'lint' })
    expect(waitOutcome({ state: 'superseded', headSha: sha, currentHeadSha: `9f1e0aa${sha.slice(7)}` })).toMatchObject({ tone: 'warning', detail: '0b2ddce → 9f1e0aa' })
    expect(waitOutcome({ state: 'closed', prNumber: 193, pullRequestState: 'MERGED' }).title).toBe('PR #193 was merged')
    expect(waitOutcome({
      state: 'completed',
      type: 'github_workflow',
      conclusion: 'cancelled',
      runAttempt: 2,
    })).toEqual({ tone: 'warning', title: 'Workflow cancelled', detail: 'Attempt 2' })
    expect(waitOutcome({ state: 'completed', type: 'run_finished', status: 'failed' })).toMatchObject({ tone: 'error', title: 'The run failed' })
    expect(waitOutcome({ state: 'timed_out' }).title).toBe('Deadline reached without a result')
    expect(waitOutcome(null)).toMatchObject({ tone: 'neutral' })
  })

  it('folds the wait tool and its events into one card that ends with the resumption', () => {
    const scheduled = wakeup({ condition: checks })
    const observation = { state: 'completed', type: 'github_pr_checks', conclusion: 'success' }
    const entries = activityEntries([
      toolCall('wait_for', 'cairn_workspace'),
      event('chat.wait', scheduled as unknown as Record<string, unknown>),
      event('chat.wait_ready', observation),
      event('chat.wakeup', { ...scheduled, observation }),
    ], true)
    expect(entries).toEqual([{
      kind: 'wait',
      id: 'wait:wake',
      time: 120_000,
      wait: { wakeup: scheduled, observation, resumedAt: 240_000 },
    }])
  })

  it('folds paginated ready and resumption events into the same card', () => {
    const observation = { state: 'completed', type: 'github_pr_checks', conclusion: 'success' }
    const scheduled = wakeup({ condition: checks, observation })
    const ready = { ...event('chat.wait_ready', observation), text: 'Wait condition resolved' }
    const entries = activityEntries([ready, event('chat.wakeup', scheduled as unknown as Record<string, unknown>)], true)
    expect(entries).toHaveLength(1)
    expect(entries[0]).toMatchObject({ id: 'wait:wake', wait: { wakeup: scheduled, observation, resumedAt: expect.any(Number) } })
    const pending = activityEntries([ready], true, undefined, scheduled)
    expect(pending).toHaveLength(1)
    expect(pending[0]).toMatchObject({ id: 'wait:wake', wait: { observation } })
    expect(activityEntries([], true, undefined, scheduled)[0]).toMatchObject({ wait: { observation } })
    const scheduleOnly = event('chat.wait', wakeup({ condition: checks }) as unknown as Record<string, unknown>)
    expect(activityEntries([scheduleOnly], true, undefined, scheduled)).toMatchObject([{ id: 'wait:wake', wait: { observation } }])
  })

  it('keeps an immediately completed replacement separate from the previous condition', () => {
    const scheduled = wakeup({ condition: checks })
    const observation = {
      state: 'completed',
      type: 'github_workflow',
      runId: 99,
      conclusion: 'success',
    }
    const entries = activityEntries([
      event('chat.wait', scheduled as unknown as Record<string, unknown>),
      { ...event('chat.wait_ready', observation), text: 'Wait condition already completed' },
    ], true)
    expect(entries).toHaveLength(2)
    expect(entries[0]).toMatchObject({ id: 'wait:wake', wait: { ended: 'replaced' } })
    expect(entries[0]).not.toHaveProperty('wait.observation')
    expect(entries[1]).toMatchObject({ wait: { wakeup: null, observation } })
  })

  it('does not attach an older resolved wait to a later pending replacement', () => {
    const observation = { state: 'completed', type: 'github_pr_checks', conclusion: 'success' }
    const ready = { ...event('chat.wait_ready', observation), text: 'Wait condition resolved' }
    const later = wakeup({ id: 'later', condition: checks, createdAt: ready.createdAt + 1 })
    const entries = activityEntries([
      ready,
      event('chat.user', { text: 'Another task' }),
      event('chat.wait', later as unknown as Record<string, unknown>),
    ], true, undefined, later).filter(entry => entry.kind === 'wait')
    expect(entries).toHaveLength(2)
    expect(entries[0]).toMatchObject({ wait: { wakeup: null, observation } })
    expect(entries[1]).toMatchObject({ id: 'wait:later', wait: { wakeup: later } })
    expect(entries[1]).not.toHaveProperty('wait.observation')
  })

  it('keeps a failed wait call among the tools and recognises every provider’s name', () => {
    const entries = activityEntries([
      toolCall('mcp__cairn_workspace__sleep', 'Claude Code'),
      toolCall('cairn_workspace_sleep', 'OpenCode'),
      toolCall('sleep', 'cairn_workspace', { error: { message: 'Handle pending user input before scheduling a wakeup.' } }),
    ], true)
    expect(entries).toHaveLength(1)
    expect(entries[0]).toMatchObject({ kind: 'group', artifacts: [{ title: 'Sleep', status: 'error' }] })
  })

  it('marks a wait a message cancelled, and an immediate result as its own card', () => {
    const entries = activityEntries([
      event('chat.sleep', wakeup() as unknown as Record<string, unknown>),
      event('chat.user', { messageId: 'next', text: 'Actually, do it now' }),
      event('chat.wait_ready', { state: 'completed', type: 'run_finished', status: 'succeeded' }),
    ], true)
    expect(entries.map(entry => entry.kind)).toEqual(['wait', 'message', 'wait'])
    expect(entries[0]).toMatchObject({ wait: { ended: 'message' } })
    expect(entries[2]).toMatchObject({ wait: { wakeup: null, observation: { status: 'succeeded' } } })
  })

  it('lists waiting conversations apart, soonest first, unless paused', () => {
    const fil = filOf([
      chat('later', { unread: true, finishedAt: 1, wakeup: wakeup({ wakeAt: 9000, condition: checks }) }),
      chat('sooner', { wakeup: wakeup({ wakeAt: 5000 }) }),
      chat('paused', { paused: true, wakeup: wakeup() }),
      chat('done'),
    ], [], [])
    expect(fil.waiting.map(item => [item.title, item.kind, item.subtitle, item.condition])).toEqual([
      ['sooner', 'waiting', expect.stringMatching(/^Resumes at /), false],
      ['later', 'waiting', 'PR #193 checks', true],
    ])
    expect(fil.recent.map(item => [item.title, item.kind])).toEqual([['paused', 'paused'], ['done', 'recent']])
    expect(fil.live).toEqual([])
    expect(fil.forYou).toEqual([])
  })
})
