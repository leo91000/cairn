import type { ChatView, ChatWakeup } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import sample from '../../../tests/fixtures/activity-events.json'
import { activityEntries } from '../src/activity'
import {
  liveWakeup,
  observedTarget,
  recordedTitle,
  timeLeft,
  waitCountdown,
  waitLeft,
  waitOutcome,
  waitProgress,
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

function event(type: string, payload: Record<string, unknown>, text = ''): RunEvent {
  return {
    id: ++id,
    runId: 'chat',
    createdAt: id * 60_000,
    type,
    text,
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
    // Seen only in a run's history, a wait reads as what was scheduled.
    expect(recordedTitle(wakeup())).toMatch(/^Scheduled to resume at /)
    expect(recordedTitle(wakeup({ condition: checks }))).toBe('Wait scheduled · PR #193 checks')
  })

  it('counts down to a timer, and to the deadline of a condition', () => {
    expect(timeLeft(45_000)).toBe('45 s')
    expect(timeLeft(61_000)).toBe('2 min')
    expect(timeLeft(65 * 60_000)).toBe('1 h 05')
    expect(waitCountdown(wakeup(), 600_000)).toBe('in 20 min')
    expect(waitCountdown(wakeup({ condition: checks }), 600_000)).toBe('up to 20 min')
    expect(waitCountdown(wakeup(), 2_000_000)).toBe('Resuming soon')
    expect(waitStatus(wakeup(), 600_000)).toBe('Resumes in 20 min')
    expect(waitStatus(wakeup({ condition: checks }), 600_000)).toBe('Awaiting result · up to 20 min')
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
    expect(waitOutcome({ state: 'completed', type: 'github_workflow', conclusion: 'failure' })).toEqual({ tone: 'error', title: 'Workflow failed', detail: '' })
    expect(waitOutcome({ state: 'completed', type: 'github_workflow', conclusion: 'startup_failure' }).detail).toBe('startup failure')
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
      event('chat.wait_ready', { state: 'completed', type: 'run_finished', status: 'succeeded' }, 'Wait condition already completed'),
    ], true)
    expect(entries.map(entry => entry.kind)).toEqual(['wait', 'message', 'wait'])
    expect(entries[0]).toMatchObject({ wait: { ended: 'cancelled' } })
    expect(entries[2]).toMatchObject({ wait: { wakeup: null, observation: { status: 'succeeded' } } })
  })

  it('keeps an immediate result apart from the wait it replaced', () => {
    const replaced = wakeup({ condition: { type: 'run_finished', runId: 'first' } })
    const immediate = { state: 'completed', type: 'run_finished', status: 'succeeded' }
    const entries = activityEntries([
      event('chat.wait', replaced as unknown as Record<string, unknown>),
      event('chat.wait_ready', immediate, 'Wait condition already completed'),
    ], true)
    expect(entries.map(entry => entry.kind === 'wait' && entry.wait)).toEqual([
      { wakeup: replaced, ended: 'replaced' },
      { wakeup: null, observation: immediate },
    ])
    // Its hidden call still gives the card the agent's reason, whichever way the provider records it.
    for (const args of [{ reason: 'Merge after CI' }, JSON.stringify({ reason: 'Merge after CI' })]) {
      const [card] = activityEntries([
        toolCall('wait_for', 'cairn_workspace', { arguments: args }),
        event('chat.wait_ready', immediate, 'Wait condition already completed'),
      ], true)
      expect(card).toMatchObject({ kind: 'wait', wait: { wakeup: null, reason: 'Merge after CI' } })
    }

    // A call announced without its arguments never borrows an earlier call's reason.
    const later = activityEntries([
      toolCall('sleep', 'cairn_workspace', { arguments: { reason: 'Earlier delay' } }),
      event('chat.sleep', wakeup() as unknown as Record<string, unknown>),
      event('chat.wakeup', wakeup() as unknown as Record<string, unknown>),
      toolCall('wait_for', 'cairn_workspace', { arguments: {} }),
      event('chat.wait_ready', immediate, 'Wait condition already completed'),
    ], true)
    expect(later.at(-1)).toMatchObject({ kind: 'wait', wait: { wakeup: null } })
    expect(later.at(-1)).not.toHaveProperty('wait.reason')
    // A call's later update cannot lend its reason again once a card used it.
    const call = (type: string, callId: string, reason: string) => event(type, {
      item: {
        id: callId,
        type: 'mcp_tool_call',
        tool: 'wait_for',
        server: 'cairn_workspace',
        status: type === 'item.started' ? 'in_progress' : 'completed',
        arguments: { reason },
      },
    })
    const raced = activityEntries([
      call('item.started', 'c1', 'First'),
      event('chat.wait_ready', immediate, 'Wait condition already completed'),
      call('item.completed', 'c1', 'First'),
      event('chat.wait_ready', immediate, 'Wait condition already completed'),
      call('item.started', 'c2', 'Second'),
    ], true)
    expect(raced.map(entry => entry.kind === 'wait' ? entry.wait.reason : undefined)).toEqual(['First', undefined])
    // Recorded with the result, the reason no longer depends on when the call is reported.
    const recorded = activityEntries([
      event('chat.wait_ready', { ...immediate, reason: 'First' }, 'Wait condition already completed'),
      call('item.started', 'c1', 'First'),
      event('chat.wait_ready', { ...immediate, reason: 'Second' }, 'Wait condition already completed'),
      call('item.started', 'c2', 'Second'),
    ], true)
    expect(recorded.map(entry => entry.kind === 'wait' ? entry.wait.reason : undefined)).toEqual(['First', 'Second'])
    // Item IDs may repeat in a later turn, whose call lends its own reason again.
    const repeated = activityEntries([
      call('item.started', 'item_1', 'First'),
      event('chat.wait', wakeup() as unknown as Record<string, unknown>),
      event('chat.wakeup', wakeup() as unknown as Record<string, unknown>),
      event('turn.started', {}),
      call('item.started', 'item_1', 'Second'),
      event('chat.wait_ready', immediate, 'Wait condition already completed'),
    ], true)
    expect(repeated.at(-1)).toMatchObject({ kind: 'wait', wait: { wakeup: null, reason: 'Second' } })
    // A resumption whose scheduling is unloaded keeps one card, even if it repeats.
    const resumed = activityEntries([
      event('chat.wakeup', wakeup() as unknown as Record<string, unknown>),
      event('chat.wakeup', wakeup() as unknown as Record<string, unknown>),
    ], true)
    expect(resumed.map(entry => entry.id)).toEqual(['wait:wake'])
  })

  it('cancels the wait of a run that ends failed, not of a turn that is retried, and lists answered waits first', () => {
    const retried = activityEntries([
      event('chat.sleep', wakeup() as unknown as Record<string, unknown>),
      event('turn.failed', { error: { message: 'Provider unavailable' } }),
    ], true)
    expect(retried[0]).toMatchObject({ kind: 'wait', wait: { wakeup: wakeup() } })
    expect(retried[0]).not.toHaveProperty('wait.ended')
    const failed = activityEntries([
      event('chat.sleep', wakeup() as unknown as Record<string, unknown>),
      { ...event('status', {}), text: 'failed' },
    ], true)
    expect(failed[0]).toMatchObject({ kind: 'wait', wait: { ended: 'cancelled' } })
    const fil = filOf([
      chat('sooner', { wakeup: wakeup({ wakeAt: 5000 }) }),
      chat('answered', { wakeup: wakeup({ wakeAt: 9000, condition: checks, observation: { state: 'completed' } }) }),
    ], [], [])
    expect(fil.waiting.map(item => [item.title, item.resolved])).toEqual([['answered', true], ['sooner', false]])
  })

  it('trusts the live wakeup over the history, and keeps a paused or answered wait pausable', () => {
    const pending = wakeup()
    const live = { wakeup: pending, paused: false, settled: true }
    // Whatever the history says, the live wakeup is authoritative: the conversation still waits.
    expect(waitProgress({ wakeup: pending, ended: 'cancelled' }, live)).toMatchObject({
      current: true,
      cancelled: false,
      pending: true,
      pausable: true,
    })
    // A stop leaves no event: the live state alone says it is over.
    expect(waitProgress({ wakeup: pending }, { wakeup: null, paused: false, settled: true })).toMatchObject({ cancelled: true, pending: false, pausable: false })
    expect(waitProgress({ wakeup: pending }, { wakeup: pending, paused: true, settled: true })).toMatchObject({ paused: true, pending: false, pausable: false })
    const answered = { ...pending, observation: { state: 'completed' } }
    expect(waitProgress({ wakeup: pending }, { wakeup: answered, paused: false, settled: true })).toMatchObject({
      finished: true,
      pending: false,
      pausable: true,
      observation: { state: 'completed' },
    })
    expect(waitProgress({ wakeup: pending, ended: 'replaced' }, { wakeup: wakeup({ id: 'next' }), paused: false, settled: true })).toMatchObject({ replaced: true, cancelled: false })
    // Without live state, as in a run's history, only the history speaks.
    expect(waitProgress({ wakeup: pending, ended: 'cancelled' })).toMatchObject({ cancelled: true, pending: false })
    expect(observedTarget({ state: 'completed', type: 'github_pr_checks', prNumber: 193 })).toBe('PR #193 checks')
    expect(observedTarget({ state: 'timed_out' })).toBe('')
    expect(waitLeft({ wakeAt: 600_000, condition: true }, 0)).toBe('≤ 10 min')
    expect(waitLeft({ wakeAt: 45_000 }, 0)).toBe('< 1 min')
    expect(waitLeft({ wakeAt: 45_000, condition: true }, 0)).toBe('< 1 min')
    // Until the turn that scheduled it ends, the conversation is not paused from the card.
    expect(waitProgress({ wakeup: pending }, { ...live, settled: false })).toMatchObject({ pending: true, pausable: false })
    expect(waitLeft({ wakeAt: 600_000, resolved: true }, 0)).toBe('soon')
    expect(waitLeft({ wakeAt: 600_000 }, 700_000)).toBe('soon')
  })

  it('says when an overdue or answered wait resumes, and ignores a wakeup its failed turn will never honour', () => {
    expect(waitStatus(wakeup(), 600_000)).toBe('Resumes in 20 min')
    expect(waitStatus(wakeup({ condition: checks }), 600_000)).toBe('Awaiting result · up to 20 min')
    expect(waitStatus(wakeup(), 2_000_000)).toBe('Resuming soon')
    expect(waitStatus(wakeup({ condition: checks, observation: { state: 'completed' } }), 600_000)).toBe('Resuming soon')
    const waiting = { ...chat('chat', { wakeup: wakeup() }), run: { status: 'succeeded' } }
    expect(liveWakeup(waiting)).toEqual(wakeup())
    expect(liveWakeup({ ...waiting, run: { status: 'failed' } })).toBeNull()
    expect(liveWakeup({ ...waiting, lifecycle: 'trash' })).toBeNull()
    expect(waitOutcome({
      state: 'completed',
      type: 'run_finished',
      status: 'succeeded',
      outcomeStatus: 'needs_input',
    }).detail).toBe('Needs your input')
    expect(waitOutcome({
      state: 'completed',
      type: 'run_finished',
      status: 'succeeded',
      outcomeStatus: 'completed',
    }).detail).toBe('')
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

describe('shared wait_ready corpus', () => {
  const immediate = [true, true, false, false, true, false]
  for (const [index, raw] of sample.slice(15).entries()) {
    it(`normalizes case ${index + 1} and preserves the pending wait`, () => {
      const ready: RunEvent = {
        id: 2,
        runId: 'chat',
        createdAt: 2,
        type: raw.type,
        text: 'text' in raw ? raw.text! : '',
        payload: 'payload' in raw ? raw.payload : undefined,
      }
      const pending = wakeup({ condition: checks })
      const scheduled: RunEvent = {
        id: 1,
        runId: 'chat',
        createdAt: 1,
        type: 'chat.wait',
        text: '',
        payload: { ...pending },
      }
      const waits = activityEntries([scheduled, ready], true).filter(entry => entry.kind === 'wait')
      expect(waits).toHaveLength(immediate[index] ? 2 : 1)
      expect(waits[0]!.wait.ended).toBe(immediate[index] ? 'replaced' : undefined)
      const result = waits.at(-1)!.wait
      expect(result.wakeup?.id ?? null).toBe(immediate[index] ? null : pending.id)
      expect(result.observation).toMatchObject({ state: 'completed', type: 'run_finished', status: 'succeeded' })
      if (immediate[index])
        expect(result.reason).toBe('Review finished')
    })
  }
})
