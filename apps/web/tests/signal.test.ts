import type { ChatView } from '../../../packages/contracts/chats'
import type { RunEvent, RunListItem, Task } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import {
  backgroundWait,
  filOf,
  identityColor,
  liveElapsed,
  shortAge,
  waitingStep,
  workingStep,
} from '../src/signal'

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
    status: 'idle',
    ...values,
  }
}

function task(id: string, values: Partial<Task> = {}): Task {
  return {
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
    expect(fil.live).toMatchObject([{ title: 'triage', kind: 'running', to: '/runs/triage-1' }])
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
    // Kotlin: Math.floorMod("cairn".hashCode(), 8) == 107030 % 8 == 6
    expect(identityColor('cairn')).toBe('#7A4FD1')
    expect(identityColor('main')).toBe(identityColor('main'))
  })
})
