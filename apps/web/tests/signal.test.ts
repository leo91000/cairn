import type { ChatView } from '../../../packages/contracts/chats'
import type { RunEvent, RunListItem, Task } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import { exclusiveSuspensionLabel } from '../src/run-status'
import {
  backgroundWait,
  dismissible,
  filOf,
  identityColor,
  liveElapsed,
  shortAge,
  tomorrowMorning,
  waitingStep,
  workingStep,
} from '../src/signal'

function chat(id: string, values: Partial<ChatView> = {}): ChatView {
  return {
    questions: [],
    run: null,
    messages: [],
    slug: '',
    revision: '',
    lifecycle: 'active',
    sessionRestartRequested: false,
    canRequestNewSession: false,
    unread: false,

    id,
    title: id,
    agentId: 'main',
    projectId: null,
    runId: null,
    paused: false,
    createdAt: 1,
    updatedAt: 1,
    pendingQuestions: 0,
    pendingSideQuestions: 0,
    canAskSideQuestion: false,
    sideQuestions: [],
    agentName: 'Cairn',
    projectName: null,
    status: 'idle',
    ...values,
  }
}

function task(id: string, values: Partial<Task> = {}): Task {
  return {
    revision: 'fixture',
    authorId: null,
    authorAccessId: null,
    authorRemoved: false,
    scheduleWaitReason: null,
    scheduleFailure: null,
    nativePrincipals: [],
    nativeAuthorityNodes: [],
    nativeParentRunId: null,
    nativeParentAccess: null,
    nativeProvenanceUnknown: true,
    nativePluginGrants: null,
    id,
    name: id,
    prompt: 'Do it',
    agentId: 'ops',
    projectId: null,
    skills: null,
    tags: [],
    cron: null,
    timezone: 'Europe/Paris',
    enabled: true,
    archived: false,
    worktree: true,
    createdAt: 1,
    nextRun: null,
    ...values,
  }
}

function run(id: string, taskId: string, values: Partial<RunListItem> = {}): RunListItem {
  return {
    id,
    taskId,
    projectId: null,
    status: 'succeeded',
    trigger: 'manual',
    createdAt: 1,
    startedAt: 1,
    finishedAt: 2,
    sessionId: null,
    workspace: null,
    usage: null,
    taskName: taskId,
    agentName: 'Ops',
    isolated: false,
    workspaces: [],
    unread: false,
    recoveryPending: false,
    resumeAvailable: false,
    resumeCount: 0,
    ...values,
  }
}

function command(id: number, at: number, value: string, running = false): RunEvent {
  return {
    id,
    runId: 'chat',
    createdAt: at,
    type: running ? 'item.started' : 'item.completed',
    text: '',
    payload: {
      item: {
        id: `tool-${id}`,
        type: 'command_execution',
        command: value,
        ...(running ? { status: 'in_progress' } : { exit_code: 0 }),
      },
    },
  }
}

function user(id: number, at: number): RunEvent {
  return {
    id,
    runId: 'chat',
    createdAt: at,
    type: 'chat.user',
    text: 'Check the tests',
    payload: { messageId: `m${id}`, text: 'Check the tests' },
  }
}

function waiting(id: number, at: number, descriptions: string[]): RunEvent {
  return {
    id,
    runId: 'chat',
    createdAt: at,
    type: 'turn.waiting',
    text: '',
    payload: { type: 'turn.waiting', tasks: descriptions.map((description, index) => ({ id: `task-${index}`, description })) },
  }
}

describe('the Fil', () => {
  const reservation: NonNullable<RunListItem['suspendedByExclusive']> = {
    id: 'reservation',
    nodeId: 'node',
    ownerRunId: 'owner',
    reason: 'Compile',
    state: 'acquired',
    createdAt: 1,
    expiresAt: 1000,
    waitUntil: 1000,
    pausedRunIds: ['chat-run', 'mission-run'],
    minimumAvailableMemoryMiB: 0,
  }

  it('keeps suspended work in Live, with failures, questions and user pauses taking priority', () => {
    const fil = filOf([
      chat('suspended', { status: 'running', suspendedByExclusive: reservation }),
      chat('failed', { status: 'failed', suspendedByExclusive: reservation }),
      chat('question', { status: 'running', pendingQuestions: 1, suspendedByExclusive: reservation }),
      chat('paused', { status: 'running', paused: true, suspendedByExclusive: reservation }),
    ], [task('compile')], [run('mission-run', 'compile', { status: 'running', finishedAt: null, suspendedByExclusive: reservation })])
    expect(fil.live.map(item => [item.title, item.kind, item.subtitle])).toEqual([
      ['suspended', 'suspended', exclusiveSuspensionLabel],
      ['compile', 'suspended', exclusiveSuspensionLabel],
    ])
    expect(fil.forYou.map(item => item.kind)).toEqual(['failed', 'question'])
    expect(fil.recent.map(item => item.kind)).toEqual(['paused'])
  })

  it('keeps settled mission review visible after an earlier exclusive suspension', () => {
    const fil = filOf([], [task('compile')], [run('mission-run', 'compile', {
      status: 'succeeded',
      suspendedByExclusive: reservation,
      outcome: {
        status: 'blocked',
        reason: 'Needs a device',
        evidence: [],
        reportedAt: 2,
      },
    })])
    expect(fil.live).toEqual([])
    expect(fil.forYou[0]).toMatchObject({ kind: 'review', subtitle: 'Blocked · Needs a device' })
  })

  it('puts what needs the user first, then live work, then recent conversations', () => {
    const fil = filOf([
      chat('recent', { updatedAt: 5 }),
      chat('question', { pendingQuestions: 1, status: 'running', updatedAt: 2 }),
      chat('working', { status: 'running', updatedAt: 3 }),
      chat('failed', { status: 'failed', updatedAt: 4 }),
      chat('paused', { status: 'running', paused: true, updatedAt: 6 }),
      chat('queued', { status: 'queued', updatedAt: 7 }),
      chat('deleted', { status: 'failed', lifecycle: 'trash' }),
    ], [], [])
    expect(fil.forYou.map(item => [item.title, item.kind])).toEqual([['failed', 'failed'], ['question', 'question']])
    expect(fil.live.map(item => [item.title, item.kind])).toEqual([['queued', 'queued'], ['working', 'running']])
    expect(fil.recent.map(item => [item.title, item.kind])).toEqual([['paused', 'paused'], ['recent', 'recent']])
    expect(fil.forYou[1]).toMatchObject({ to: '/chats/question', subtitle: 'Needs your answer' })
  })

  it('marks each conversation and mission with the coding agent of its latest turn', () => {
    const fil = filOf([
      chat('switched', { status: 'running', provider: 'claude' }),
      chat('new', { provider: null }),
    ], [task('audit')], [run('audit-run', 'audit', { status: 'running', finishedAt: null, provider: 'opencode' })])
    expect([...fil.live, ...fil.recent].map(item => [item.title, item.harness])).toEqual([
      ['switched', 'claude'],
      ['audit', 'opencode'],
      ['new', undefined],
    ])
  })

  it('reports each mission from its latest run and leaves chat runs to their conversation', () => {
    const fil = filOf([], [task('audit'), task('triage'), task('digest'), task('old', { archived: true }), task('never')], [
      run('audit-1', 'audit', { status: 'failed', createdAt: 10, error: 'Tests failed\nstack' }),
      run('audit-0', 'audit', { status: 'succeeded', createdAt: 5 }),
      run('triage-1', 'triage', {
        status: 'running',
        createdAt: 12,
        startedAt: 12,
        finishedAt: null,
      }),
      run('digest-1', 'digest', {
        outcome: {
          status: 'blocked',
          reason: 'Needs a GitHub token',
          evidence: [],
          reportedAt: 3,
        },
        createdAt: 3,
      }),
      run('old-1', 'old', { status: 'failed' }),
      run('chat-1', 'digest', { status: 'failed', trigger: 'chat', createdAt: 99 }),
    ])
    expect(fil.forYou.map(item => [item.title, item.kind, item.subtitle])).toEqual([
      ['audit', 'failed-mission', 'Tests failed'],
      ['digest', 'review', 'Blocked · Needs a GitHub token'],
    ])
    // Missions open beside the Fil, like conversations.
    expect(fil.live).toMatchObject([{
      title: 'triage',
      kind: 'running',
      to: '/fil/runs/triage-1',
      runId: 'triage-1',
    }])
  })

  it('keeps finished work the user has not read in For you, below what needs an answer', () => {
    const fil = filOf([
      chat('done', {
        status: 'succeeded',
        unread: true,
        finishedAt: 40,
        updatedAt: 30,
        preview: 'Shipped the notes.',
      }),
      chat('read', { status: 'succeeded', unread: false, finishedAt: 50 }),
      chat('question', {
        pendingQuestions: 1,
        status: 'succeeded',
        unread: true,
        updatedAt: 2,
      }),
      chat('paused', { status: 'succeeded', paused: true, unread: false }),
    ], [task('digest'), task('audit')], [
      run('digest-1', 'digest', { finishedAt: 60, unread: true, preview: 'Three stories today.' }),
      run('audit-1', 'audit', {
        finishedAt: 70,
        unread: true,
        outcome: {
          status: 'needs_input',
          reason: 'Pick a scope',
          evidence: [],
          reportedAt: 70,
        },
      }),
    ])
    expect(fil.forYou.map(item => [item.title, item.kind])).toEqual([
      ['audit', 'review'],
      ['question', 'question'],
      ['digest', 'unread'],
      ['done', 'unread'],
    ])
    expect(fil.forYou[3]).toMatchObject({ at: 40, subtitle: 'Finished', preview: 'Shipped the notes.' })
    expect(fil.forYou[2]).toMatchObject({ key: 'task:digest', subtitle: 'Mission finished', preview: 'Three stories today.' })
    expect(fil.recent.map(item => item.title).sort()).toEqual(['paused', 'read'])
  })

  it('lists a mission that scheduled its continuation with waiting conversations, soonest first', () => {
    const wakeup = (wakeAt: number) => ({
      id: `wake-${wakeAt}`,
      messageId: null,
      seconds: 60,
      reason: 'Wait for CI',
      createdAt: 1,
      wakeAt,
    })
    const fil = filOf([
      chat('later', { status: 'succeeded', unread: false, wakeup: wakeup(900) }),
    ], [task('watch')], [
      run('watch-1', 'watch', { status: 'waiting', finishedAt: null, wakeup: wakeup(300) }),
    ])
    expect(fil.live).toEqual([])
    expect(fil.waiting.map(item => [item.title, item.kind, item.wakeAt])).toEqual([
      ['watch', 'waiting', 300],
      ['later', 'waiting', 900],
    ])
    expect(fil.waiting[0]).toMatchObject({ key: 'task:watch', subtitle: 'Wait for CI', runId: 'watch-1' })
  })

  it('places automatic quota waits after dated resumptions and keeps attention and suspension priorities', () => {
    const quota = { status: 'running' as const, accountWaitReason: 'Usage exhausted. Waiting for an available Codex account to resume.' }
    const fil = filOf([
      chat('quota', quota),
      chat('admission', { status: 'queued', accountWaitReason: 'Waiting for a Claude Code account with available usage.' }),
      chat('question', { ...quota, pendingQuestions: 1 }),
      chat('paused', { ...quota, paused: true }),
      chat('failed', { ...quota, status: 'failed' }),
      chat('reconnect', { ...quota, accountRequired: 'codex' }),
      chat('suspended', { ...quota, suspendedByExclusive: reservation }),
      chat('runner', { status: 'queued', accountWaitReason: 'Waiting for a free runner.' }),
      chat('dated', {
        status: 'succeeded',
        wakeup: {
          id: 'wake',
          messageId: null,
          reason: '',
          seconds: 60,
          createdAt: 1,
          wakeAt: 300,
        },
      }),
    ], [task('mission')], [run('mission-1', 'mission', quota)])
    expect(fil.waiting.map(item => item.title)).toEqual(['dated', 'quota', 'admission', 'mission'])
    expect(fil.waiting.slice(1).every(item => item.subtitle === 'Waiting for quota' && item.wakeAt === undefined)).toBe(true)
    expect(fil.forYou.map(item => item.title)).toEqual(['question', 'failed'])
    expect(fil.recent.map(item => item.title)).toEqual(['paused'])
    expect(fil.live.map(item => item.title)).toEqual(['reconnect', 'suspended', 'runner'])
    expect(filOf([chat('quota', { ...quota, accountWaitReason: null })], [], []).live[0].kind).toBe('running')
  })

  describe('a blocked mission', () => {
    const blocked = (values: Partial<RunListItem['outcome'] & object> = {}): RunListItem['outcome'] => ({
      status: 'blocked',
      reason: 'CI belongs to the parent',
      evidence: [],
      reportedAt: 2,
      ...values,
    })

    it('goes back to the agent work that started it when it waits on that work', () => {
      const fil = filOf([chat('orchestrate', { title: 'Orchestrate F8' })], [task('proof'), task('review'), task('orphan')], [
        run('proof-1', 'proof', { outcome: blocked({ waitingOn: 'requester' }), requestedBy: { runId: 'chat-run', chatId: 'orchestrate' } }),
        run('review-1', 'review', { outcome: blocked({ waitingOn: 'requester' }), requestedBy: { runId: 'proof-1', taskId: 'proof' } }),
        // Its requester is gone: the user is the one left to act.
        run('orphan-1', 'orphan', { outcome: blocked({ waitingOn: 'requester' }), requestedBy: { runId: 'gone', chatId: 'deleted' } }),
      ])
      expect(fil.recent.map(item => [item.title, item.kind, item.setAside, item.subtitle, item.requester?.to])).toEqual([
        ['proof', 'set-aside', 'requester', 'Waiting on Orchestrate F8', '/chats/orchestrate'],
        ['review', 'set-aside', 'requester', 'Waiting on proof', '/fil/runs/proof-1'],
        ['Orchestrate F8', 'recent', undefined, 'Cairn', undefined],
      ])
      expect(fil.forYou.map(item => [item.title, item.kind])).toEqual([['orphan', 'review']])
    })

    it('waits in Recent on an outside check, and asks the user by default', () => {
      const fil = filOf([], [task('ci'), task('token')], [
        run('ci-1', 'ci', { outcome: blocked({ waitingOn: 'external' }) }),
        run('token-1', 'token', { outcome: blocked({ waitingOn: 'user' }) }),
      ])
      expect(fil.recent).toMatchObject([{
        title: 'ci',
        kind: 'set-aside',
        setAside: 'external',
        outcome: 'blocked',
      }])
      expect(fil.forYou).toMatchObject([{ title: 'token', kind: 'review', outcome: 'blocked' }])
    })

    it('leaves For you once dismissed, and waits until its snooze ends', () => {
      const runs = [
        run('dismissed-1', 'dismissed', { outcome: blocked(), dismissal: { until: null } }),
        run('snoozed-1', 'snoozed', { outcome: blocked({ status: 'needs_input', reason: 'Pick a scope' }), dismissal: { until: 100 } }),
      ]
      const tasks = [task('dismissed'), task('snoozed')]
      const before = filOf([], tasks, runs, {}, 50)
      expect(before.recent).toMatchObject([{ title: 'dismissed', kind: 'set-aside', setAside: 'dismissed' }])
      expect(before.waiting).toMatchObject([{
        title: 'snoozed',
        kind: 'snoozed',
        wakeAt: 100,
        subtitle: 'Snoozed · Needs your input · Pick a scope',
      }])
      expect(before.forYou).toEqual([])
      expect(filOf([], tasks, runs, {}, 100).forYou.map(item => [item.title, item.kind])).toEqual([['snoozed', 'review']])
    })

    it('can be dismissed only when blocked: input the agent asked for can only be snoozed', () => {
      const fil = filOf([], [task('blocked'), task('input')], [
        run('blocked-1', 'blocked', { outcome: blocked(), createdAt: 2 }),
        run('input-1', 'input', { outcome: blocked({ status: 'needs_input' }) }),
      ])
      expect(fil.forYou.map(item => [item.title, dismissible(item)])).toEqual([['blocked', true], ['input', false]])
    })

    it('asks again tomorrow at nine, local time', () => {
      const evening = new Date(2026, 9, 20, 22, 30).getTime()
      expect(new Date(tomorrowMorning(evening))).toEqual(new Date(2026, 9, 21, 9, 0))
    })
  })

  it('treats work read on this page as read until the server catches up, but not a later turn', () => {
    const chats = [chat('done', { status: 'succeeded', unread: true, finishedAt: 40 })]
    expect(filOf(chats, [], [], { 'chat:done': 40 }).recent.map(item => item.title)).toEqual(['done'])
    expect(filOf(chats, [], [], { 'chat:done': 39 }).forYou.map(item => item.kind)).toEqual(['unread'])
  })
})

describe('the working indicator', () => {
  it('names the step in progress with its command, timed from the latest message', () => {
    const entries = activityEntries([user(1, 5000), command(2, 5100, 'cat README.md'), command(3, 5200, 'pnpm test', true)], true)
    expect(workingStep(entries, 'Cairn', 10)).toEqual({ title: expect.any(String), detail: 'pnpm test', since: 5000 })
  })

  it('says the agent is working with its last step, ignoring earlier turns', () => {
    const entries = activityEntries([command(1, 1000, 'pnpm build', true), user(2, 7000)], true)
    expect(workingStep(entries, '', 3000)).toEqual({ title: 'The agent is working', detail: '', since: 7000 })
    const done = activityEntries([user(1, 1), command(2, 2, 'pnpm lint')], true)
    expect(workingStep(done, 'Cairn', null).detail).toMatch(/^Last step: /)
    expect(workingStep([], 'Cairn', 3000)).toEqual({ title: 'Cairn is working', detail: '', since: 3000 })
  })

  it('shows an idle agent waiting for its background tasks, timed from when it began waiting', () => {
    const events = [user(1, 1000), command(2, 2000, 'pnpm test > log 2>&1'), waiting(3, 9000, ['Rebuild and run regression test'])]
    const wait = backgroundWait(events)
    expect(wait).toEqual({ tasks: ['Rebuild and run regression test'], since: 9000 })
    expect(waitingStep(wait!)).toEqual({
      title: 'Waiting for a background task',
      detail: 'Rebuild and run regression test',
      since: 9000,
      waiting: ['Rebuild and run regression test'],
    })
    expect(waitingStep({ tasks: ['Build', ''], since: 1 }).title).toBe('Waiting for 2 background tasks')
    expect(backgroundWait([waiting(1, 1, [' ', 'Watch CI'])])?.tasks).toEqual(['Background task', 'Watch CI'])
  })

  it('keeps the original wait duration when tasks change and resets after the agent resumes', () => {
    const events = [waiting(1, 1000, ['Build', 'Watch CI']), waiting(2, 2000, ['Watch CI'])]
    expect(backgroundWait(events)).toEqual({ tasks: ['Watch CI'], since: 1000 })
    expect(backgroundWait([...events, command(3, 3000, 'cat log', true), waiting(4, 4000, ['Watch CI'])])).toEqual({ tasks: ['Watch CI'], since: 4000 })
    expect(backgroundWait([...events, waiting(3, 3000, []), waiting(4, 4000, ['Build'])])).toEqual({ tasks: ['Build'], since: 4000 })
  })

  it('stops waiting once the tasks finish, the agent acts again or the user writes', () => {
    const announced = [user(1, 1000), waiting(2, 2000, ['Build'])]
    expect(backgroundWait([])).toBeNull()
    expect(backgroundWait([...announced, waiting(3, 3000, [])])).toBeNull()
    expect(backgroundWait([...announced, command(3, 3000, 'cat log', true)])).toBeNull()
    expect(backgroundWait([...announced, user(3, 3000)])).toBeNull()
    expect(backgroundWait([...announced, { ...user(3, 3000), type: 'diagnostic' }])).not.toBeNull()
  })

  it('labels background waits in the activity log', () => {
    const [group] = activityEntries([waiting(1, 1, ['Build', 'Watch CI']), waiting(2, 2, [])])
    expect(group?.kind === 'group' && group.artifacts.map(artifact => [artifact.title, artifact.subtitle])).toEqual([
      ['Waiting for background tasks', 'Build · Watch CI'],
      ['Background tasks finished', ''],
    ])
  })

  it('shows elapsed time to the second and short ages', () => {
    expect(liveElapsed(1, 45_001)).toBe('45s')
    expect(liveElapsed(1, 124_001)).toBe('2m 04s')
    expect(liveElapsed(1, 3_900_001)).toBe('1h 05')
    expect(liveElapsed(null)).toBe('')
    expect(shortAge(0, 10_000)).toBe('now')
    expect(shortAge(0, 4 * 60_000)).toBe('4m')
    expect(shortAge(0, 3 * 3_600_000)).toBe('3h')
  })

  it('keeps identity colours stable and shared with Android', () => {
    // Kotlin: Math.floorMod("cairn".hashCode(), 8) == 94422855 % 8 == 7
    expect(identityColor('h')).toBe('#14365A') // Java hash 104 maps to palette entry 0.
    expect(identityColor('cairn')).toBe('#5E6B2E')
    expect(identityColor('main')).toBe(identityColor('main'))
  })
})

describe('delegated conversations', () => {
  it('name the conversation that delegated them while it remains', () => {
    const requester = chat('requester', { title: 'Refactor the send queue', slug: 'refactor' })
    const delegated = chat('delegated', { status: 'running', requester: { chatId: 'requester', title: 'Refactor the send queue' } })
    const fil = filOf([requester, delegated], [], [])
    expect(fil.live.find(item => item.chatId === 'delegated')?.requester).toEqual({ title: 'Refactor the send queue', to: '/chats/requester' })
    expect(fil.recent.find(item => item.chatId === 'requester')?.requester).toBeUndefined()
  })
})
