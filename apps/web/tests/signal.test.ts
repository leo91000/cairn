import type { ChatView } from '../../../packages/contracts/chats'
import type { RunEvent, RunListItem } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { exclusiveSuspensionLabel } from '../src/run-status'
import {
  backgroundWait,
  filOf,
  filThread,
  filViews,
  identityColor,
  liveElapsed,
  shortAge,
  tomorrowMorning,
  waitingStep,
  workingStep,
} from '../src/signal'
import { fixtureEntries } from './timeline-fixture'

function chat(id: string, values: Partial<ChatView> = {}): ChatView {
  return {
    questions: [],
    run: null,
    messages: [],
    slug: '',
    revision: '',
    lifecycle: 'active',
    personParticipated: false,
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
    renewalPending: false,
    error: null,
    createdAt: 1,
    expiresAt: 1000,
    waitUntil: 1000,
    pausedRunIds: ['chat-run'],
    minimumAvailableMemoryMiB: 0,
  }

  it('keeps suspended work in Live, with failures, questions and user pauses taking priority', () => {
    const fil = filOf([
      chat('suspended', { status: 'running', suspendedByExclusive: reservation }),
      chat('failed', { status: 'failed', suspendedByExclusive: reservation }),
      chat('question', { status: 'running', pendingQuestions: 1, suspendedByExclusive: reservation }),
      chat('paused', { status: 'running', paused: true, suspendedByExclusive: reservation }),
    ])
    expect(fil.live.map(item => [item.title, item.kind, item.subtitle])).toEqual([
      ['suspended', 'suspended', exclusiveSuspensionLabel],
    ])
    expect(fil.forYou.map(item => item.kind)).toEqual(['failed', 'question'])
    expect(fil.recent.map(item => item.kind)).toEqual(['paused'])
  })

  it('keeps settled conversation review visible after an earlier exclusive suspension', () => {
    const fil = filOf([chat('compile', {
      unread: true,
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
    ])
    expect(fil.forYou.map(item => [item.title, item.kind])).toEqual([['failed', 'failed'], ['question', 'question']])
    expect(fil.live.map(item => [item.title, item.kind])).toEqual([['queued', 'queued'], ['working', 'running']])
    expect(fil.recent.map(item => [item.title, item.kind])).toEqual([['paused', 'paused'], ['recent', 'recent']])
    expect(fil.forYou[1]).toMatchObject({ to: '/chats/question', subtitle: 'Needs your answer' })
  })

  it('leaves questions a member cannot answer waiting for someone with access', () => {
    const question = chat('question', { pendingQuestions: 1, status: 'running' })
    const fil = filOf([question], {}, {}, Date.now(), [])
    expect(fil.forYou).toEqual([])
    expect(fil.waiting).toMatchObject([{ title: 'question', kind: 'waiting', subtitle: 'Waiting for someone with access' }])
    expect(filOf([question], {}, {}, Date.now(), ['main']).forYou.map(item => item.kind)).toEqual(['question'])
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
      chat('audit', {
        status: 'succeeded',
        updatedAt: 70,
        finishedAt: 70,
        unread: true,
        outcome: {
          status: 'blocked',
          reason: 'Pick a scope',
          reportedAt: 70,
          evidence: [],
        },
      }),
    ])
    expect(fil.forYou.map(item => [item.title, item.kind])).toEqual([
      ['audit', 'review'],
      ['question', 'question'],
      ['done', 'unread'],
    ])
    expect(fil.forYou[2]).toMatchObject({ at: 40, subtitle: 'Finished', preview: 'Shipped the notes.' })
    expect(fil.recent.map(item => item.title).sort()).toEqual(['paused', 'read'])
  })

  it('lists dated conversation continuations, soonest first', () => {
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
      chat('watch', { status: 'succeeded', unread: false, wakeup: wakeup(300) }),
    ])
    expect(fil.live).toEqual([])
    expect(fil.waiting.map(item => [item.title, item.kind, item.wakeAt])).toEqual([
      ['watch', 'waiting', 300],
      ['later', 'waiting', 900],
    ])
    expect(fil.waiting[0]).toMatchObject({ key: 'chat:watch', wakeAt: 300 })
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
    ])
    expect(fil.waiting.map(item => item.title)).toEqual(['dated', 'quota', 'admission'])
    expect(fil.waiting.slice(1).every(item => item.subtitle === 'Waiting for quota' && item.wakeAt === undefined)).toBe(true)
    expect(fil.forYou.map(item => item.title)).toEqual(['question', 'failed'])
    expect(fil.recent.map(item => item.title)).toEqual(['paused'])
    expect(fil.live.map(item => item.title)).toEqual(['reconnect', 'suspended', 'runner'])
    expect(filOf([chat('quota', { ...quota, accountWaitReason: null })]).live[0].kind).toBe('running')
  })

  it('treats work read on this page as read until the server catches up, but not a later turn', () => {
    const chats = [chat('done', { status: 'succeeded', unread: true, finishedAt: 40 })]
    expect(filOf(chats, { 'chat:done': 40 }).recent.map(item => item.title)).toEqual(['done'])
    expect(filOf(chats, { 'chat:done': 39 }).forYou.map(item => item.kind)).toEqual(['unread'])
  })
})

describe('the working indicator', () => {
  it('names the step in progress with its command, timed from the latest message', () => {
    const entries = fixtureEntries([user(1, 5000), command(2, 5100, 'cat README.md'), command(3, 5200, 'pnpm test', true)], true)
    expect(workingStep(entries, 'Cairn', 10)).toEqual({ title: expect.any(String), detail: 'pnpm test', since: 5000 })
  })

  it('says the agent is working with its last step, ignoring earlier turns', () => {
    const entries = fixtureEntries([command(1, 1000, 'pnpm build', true), user(2, 7000)], true)
    expect(workingStep(entries, '', 3000)).toEqual({ title: 'The agent is working', detail: '', since: 7000 })
    const done = fixtureEntries([user(1, 1), command(2, 2, 'pnpm lint')], true)
    expect(workingStep(done, 'Cairn', null).detail).toMatch(/^Last step: /)
    expect(workingStep([], 'Cairn', 3000)).toEqual({ title: 'Cairn is working', detail: '', since: 3000 })
  })

  it('presents the server’s current background wait and local fallback labels', () => {
    const card = {
      id: 'notice:3',
      runId: 'run',
      position: 3,
      revision: 3,
      source: 3,
      turn: 1,
      attempt: 0,
      createdAt: 9000,
      updatedAt: 9000,
      content: {
        kind: 'notice' as const,
        notice: {
          code: 'backgroundTasks' as const,
          state: 'info' as const,
          prominent: false,
          tasks: ['Build', ' '],
          taskIds: ['one', 'two'],
          waitingSince: 1000,
        },
      },
    }
    const wait = backgroundWait([card])
    expect(wait).toEqual({ tasks: ['Build', 'Background task'], taskIds: ['one', 'two'], since: 1000 })
    expect(waitingStep(wait!).title).toBe('Waiting for 2 background tasks')
    expect(backgroundWait([{ ...card, content: { ...card.content, notice: { ...card.content.notice, waitingSince: undefined } } }])).toBeNull()
    expect(backgroundWait([])).toBeNull()
  })

  it('labels background waits in the activity log', () => {
    const [group] = fixtureEntries([waiting(1, 1, ['Build', 'Watch CI']), waiting(2, 2, [])])
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
    const fil = filOf([requester, delegated])
    expect(fil.live.find(item => item.chatId === 'delegated')?.requester).toEqual({ title: 'Refactor the send queue', to: '/chats/requester', chatId: 'requester' })
    expect(fil.recent.find(item => item.chatId === 'requester')?.requester).toBeUndefined()
  })

  it('sit right below the work that started them when both are listed', () => {
    const fil = filOf([
      chat('ship', { title: 'Ship PR 382', updatedAt: 10 }),
      chat('review', { title: 'Review R51', updatedAt: 30, requester: { chatId: 'ship', title: 'Ship PR 382' } }),
      chat('other', { title: 'Other work', updatedAt: 20 }),
      chat('orphan', { title: 'Orphan review', updatedAt: 5, requester: { chatId: 'gone', title: 'Deleted' } }),
      chat('proof', { updatedAt: 1, requester: { chatId: 'review', title: 'Review R51' } }),
    ])
    const rows = filThread(filViews(fil).all)
    expect(rows.map(({ item, nested }) => [item.title, nested])).toEqual([
      ['Other work', false],
      ['Ship PR 382', false],
      ['Review R51', true],
      ['proof', true],
      ['Orphan review', false],
    ])
  })

  it('stay in place when their requester is not listed', () => {
    const fil = filOf([
      chat('ship', { title: 'Ship PR 382', updatedAt: 10 }),
      chat('review', {
        title: 'Review R51',
        status: 'running',
        updatedAt: 30,
        requester: { chatId: 'ship', title: 'Ship PR 382' },
      }),
    ])
    expect(filThread(filViews(fil).ongoing)).toEqual([{ item: expect.objectContaining({ title: 'Review R51' }), nested: false }])
  })
})

describe('finished work', () => {
  it('is marked so its row can stay quiet, unlike work that needs attention', () => {
    const fil = filOf([
      chat('done', { title: 'done', status: 'succeeded' }),
      chat('failed', { title: 'failed', status: 'failed' }),
      chat('paused', { title: 'paused', status: 'succeeded', paused: true }),
    ])
    expect(filViews(fil).all.map(item => [item.title, !!item.finished]).sort()).toEqual([
      ['done', true],
      ['failed', false],
      ['paused', false],
    ])
  })
})

describe('compact reception tabs', () => {
  it('acknowledges questions and errors without removing their status or active work', () => {
    const chats = [
      chat('answer', { pendingQuestions: 1, unread: true, readAt: 10 }),
      chat('error', {
        status: 'failed',
        unread: true,
        finishedAt: 20,
        readAt: 20,
      }),
      chat('working', { status: 'running' }),
    ]
    const before = filViews(filOf(chats))
    expect(before.unread.map(item => item.title)).toEqual(['answer', 'error'])
    expect(before.ongoing.map(item => item.title)).toEqual(['answer', 'working'])
    const after = filViews(filOf(chats, { 'chat:answer': 10, 'chat:error': 20 }))
    expect(after.unread).toEqual([])
    expect(after.ongoing.map(item => [item.title, item.kind])).toEqual([['answer', 'question'], ['working', 'running']])
    expect(after.all.find(item => item.title === 'error')?.kind).toBe('failed')
    expect(filViews(filOf([chat('answer', { pendingQuestions: 2, unread: true, readAt: 30 })], { 'chat:answer': 10 })).unread).toHaveLength(1)
  })

  it('does not notify for completed delegated conversations', () => {
    const views = filViews(filOf([chat('delegate', {
      status: 'succeeded',
      unread: true,
      finishedAt: 30,
      requestedBy: { chatId: 'parent', runId: 'run', requestId: null },

    })]))
    expect(views.unread).toEqual([])
    expect(views.all.map(item => item.title)).toEqual(['delegate'])
  })
})

it('shows blocked conversations until read or snoozed for the account', () => {
  const outcome = {
    status: 'blocked' as const,
    waitingOn: 'external' as const,
    reason: 'Waiting for CI',
    reportedAt: 20,
    evidence: [],
  }
  const chats = [chat('schedule', {
    status: 'succeeded',
    originRule: { ruleId: 'rule', ruleName: 'CI', schedule: true },
    unread: true,
    finishedAt: 20,
    outcome,
  }), chat('question', { pendingQuestions: 1, unread: true, readAt: 30 })]
  expect(filOf(chats).forYou.map(item => item.kind)).toEqual(['review', 'question'])
  expect(filOf(chats, {}, { schedule: 100, question: 100 }, 50).waiting.map(item => item.kind)).toEqual(['snoozed', 'snoozed'])
  expect(filOf(chats, { 'chat:schedule': 20 }).recent[0]).toMatchObject({ outcome: 'blocked' })
})

it('defaults to nine tomorrow using local time', () => {
  const time = new Date(2026, 4, 1, 22, 5)
  expect(new Date(tomorrowMorning(time.getTime()))).toEqual(new Date(2026, 4, 2, 9))
})
