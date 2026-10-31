import type { RunEvent } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import { describe, expect, it } from 'vitest'
import corpus from '../../../tests/fixtures/timeline-corpus.json'
import {
  activityEntries,
  ActivityGroups,
  proposedPlanApproval,
  teamEntries,
} from '../src/activity'

// The server's UI events and timeline cards for the same three-engine corpus Android renders.
interface CorpusRun { runId: string, events: RunEvent[], entries: TimelineEntry[] }

const runs = corpus.runs as unknown as CorpusRun[]
const [codex, claude, opencode, sample] = runs as [CorpusRun, CorpusRun, CorpusRun, CorpusRun]
const events = sample.events
const feed = (run: CorpusRun, chat: boolean) => activityEntries(run.events, chat, undefined, undefined, run.entries)

function shown(run: CorpusRun) {
  return feed(run, true).map(entry => entry.kind === 'message'
    ? `${entry.role ?? 'agent'}: ${entry.text}${entry.via ? ` via ${entry.via}` : ''}`
    : entry.kind === 'notice'
      ? `notice: ${entry.artifact.title}`
      : entry.kind)
}

describe('conversation activity', () => {
  it('binds proposal approval to the server card when delivery events are only markers', () => {
    const user = codex.entries.find(card => card.content.kind === 'userMessage')!
    if (user.content.kind !== 'userMessage')
      throw new Error('Missing fixture user card')
    const card: TimelineEntry = {
      ...user,
      source: 10,
      position: 10,
      revision: 10,
      content: { ...user.content, messageId: 'clarified', plan: true },
    }
    const proposal: RunEvent = {
      id: 11,
      runId: 'chat',
      createdAt: 11,
      type: 'item.completed',
      text: '',
      payload: {
        item: {
          id: 'plan',
          type: 'proposed_plan',
          text: 'Approach',
          request_id: 'original',
        },
      },
    }
    const marker: RunEvent = {
      id: 10,
      runId: 'chat',
      createdAt: 10,
      type: 'chat.user',
      text: '',
      timeline: true,
    }
    expect(proposedPlanApproval([marker, proposal], [card])).toBeUndefined()
    proposal.payload = {
      item: {
        id: 'plan',
        type: 'proposed_plan',
        text: 'Revised approach',
        request_id: 'clarified',
      },
    }
    expect(proposedPlanApproval([marker, proposal], [card])).toBe('chat:11')
  })

  it('presents the server’s messages and failure cards in place among the tools', () => {
    expect(shown(codex)).toEqual([
      'user: Fix the build',
      // A reply takes the place where it first appeared, before the tools that ran meanwhile.
      'agent: Looking at the build. Fixed.',
      'group',
      'user: And the tests?',
      'agent: Tests pass. via OpenRouter',
      'user: Deploy it',
      'notice: The agent ran into an error after 1 automatic retry',
    ])
    // The tool's JSON-looking output stays a command, never a protocol event.
    const tools = feed(codex, true).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])
    expect(tools.map(tool => tool.kind)).toEqual(['command'])
    expect(shown(claude)).toEqual([
      // A private answer reaches the timeline only as the placeholder the event recorded.
      'user: (private answer)',
      'notice: This turn runs at Standard speed',
      'agent: Working on it',
      'notice: Claude Code warning',
      'notice: Run cancelled',
    ])
    expect(shown(opencode)).toEqual(['agent: Partial', 'notice: Runner failed', 'notice: Run interrupted'])
  })

  it('keeps every lifecycle card in the run log, with its recovery and verbatim output', () => {
    const notices = feed(codex, false).flatMap(entry => entry.kind === 'group' ? entry.artifacts.filter(item => item.kind === 'notice') : [])
    const connections = notices.filter(item => item.title === 'Connection interrupted')
    expect(connections).toHaveLength(2)
    expect(connections.every(item => item.status === 'info' && item.statusLabel === 'Recovered')).toBe(true)
    expect(notices.map(item => item.title)).toContain('Temporary error — automatic retry 1/3')
    expect(notices.map(item => item.title)).toContain('Using Codex account: Studio')
    const diagnostic = notices.at(-1)!
    expect(diagnostic).toMatchObject({ title: 'Agent diagnostic', verbatim: true, blocks: [{ label: 'Output', language: 'plaintext' }] })
    expect(diagnostic.blocks[0]!.code.startsWith('{"type":"turn.completed"}')).toBe(true)
    // The raw record of each card is the event the server read, for the expandable details.
    expect(diagnostic.raw).toBe(diagnostic.blocks[0]!.code)
    // Covered events never render twice: no lifecycle event shows as a tool step.
    expect(feed(codex, false).flatMap(entry => entry.kind === 'group' ? entry.artifacts : []).filter(item => item.kind !== 'notice')).toHaveLength(1)
  })
  it('marks plan requests and shows each proposed plan once, apart from replies', () => {
    let id = 0
    const event = (type: string, payload: Record<string, unknown>): RunEvent => ({
      id: ++id,
      runId: 'chat',
      createdAt: id,
      type,
      text: '',
      payload,
    })
    const proposal = (type: string, text: string) => event(type, {
      item: {
        id: 'exit-plan-tool',
        type: 'proposed_plan',
        text,
        status: type === 'item.started' ? 'in_progress' : 'completed',
      },
    })
    const planEvents = [
      event('chat.user', { messageId: 'plan', text: 'Plan the composer change', plan: true }),
      proposal('item.started', '1. Read'),
      proposal('item.completed', '1. Read\n2. Change'),
      event('item.completed', { item: { id: 'reply', type: 'agent_message', text: 'Plan ready for review' } }),
      event('chat.user', { messageId: 'implement', text: 'Implement the plan you proposed.' }),
    ]
    const cards: TimelineEntry[] = planEvents.flatMap((event) => {
      const item = event.payload?.item as Record<string, unknown> | undefined
      if (event.type !== 'chat.user' && item?.type !== 'agent_message')
        return []
      const user = event.type === 'chat.user'
      return [{
        id: user ? `user:${event.id}` : `reply:${event.id}`,
        runId: event.runId,
        position: event.id,
        revision: event.id,
        source: event.id,
        turn: 0,
        attempt: 0,
        createdAt: event.createdAt,
        updatedAt: event.createdAt,
        content: user
          ? {
              kind: 'userMessage' as const,
              text: String(event.payload?.text ?? ''),
              sentAt: event.createdAt,
              attachments: [],
              ...(event.payload?.plan === true ? { plan: true } : {}),
            }
          : {
              kind: 'agentMessage' as const,
              itemId: String(item?.id),
              text: String(item?.text),
              state: 'done' as const,
            },
      }]
    })
    const entries = activityEntries(planEvents.map(event => event.type === 'chat.user' || (event.payload?.item as Record<string, unknown> | undefined)?.type === 'agent_message'
      ? {
          ...event,
          timeline: true,
          text: '',
          payload: undefined,
        }
      : event), true, undefined, undefined, cards)
    expect(entries.map(entry => entry.kind)).toEqual(['message', 'proposal', 'message', 'message'])
    expect(entries[0]).toMatchObject({ role: 'user', plan: true })
    expect(entries[1]).toMatchObject({ kind: 'proposal', text: '1. Read\n2. Change' })
    // Older events and ordinary requests carry no plan marker.
    expect(entries[3]).not.toHaveProperty('plan')
  })
  it('keeps proposal approval identity exclusive to its completed lifecycle event', () => {
    const event = (id: number, type: string, text: string): RunEvent => ({
      id,
      runId: 'run',
      createdAt: id,
      type,
      text: '',
      payload: { item: { id: 'proposal', type: 'proposed_plan', text } },
    })
    const started = event(1, 'item.started', 'Partial design')
    expect(activityEntries([started], true)[0]).toMatchObject({ kind: 'proposal', text: 'Partial design' })
    expect(activityEntries([started], true)[0]).not.toHaveProperty('sourceId', 'run:1')
    const completed = event(2, 'item.completed', 'Reviewed complete design')
    expect(activityEntries([started, completed], true)[0]).toMatchObject({ kind: 'proposal', sourceId: 'run:2', text: 'Reviewed complete design' })
    const reopened = event(3, 'item.started', 'Revising design')
    expect(activityEntries([started, completed, reopened], true)[0]).not.toHaveProperty('sourceId', 'run:2')
  })
  it('invalidates approval on an empty root lifecycle and restores it only on completion', () => {
    const proposal = (id: number, type: string, name: string, content: string): RunEvent => ({
      id,
      runId: 'run',
      createdAt: id,
      type,
      text: '',
      payload: { item: { id: name, type: 'proposed_plan', text: content } },
    })
    const completed = proposal(10, 'item.completed', 'old', 'Reviewed')
    for (const name of ['old', 'new']) {
      const empty = proposal(11, 'item.started', name, '')
      expect(proposedPlanApproval([completed, empty])).toBeUndefined()
      const finished = proposal(12, 'item.completed', name, 'Updated approach')
      expect(proposedPlanApproval([completed, empty, finished])).toBe('run:12')
    }
  })
  it('does not renew an authored request boundary when a proposal is replayed', () => {
    const user = (id: number, messageId: string): RunEvent => ({
      id,
      runId: 'run',
      createdAt: id,
      type: 'chat.user',
      text: '',
      payload: { messageId },
    })
    const proposal = (id: number, requestId: string | null): RunEvent => ({
      id,
      runId: 'run',
      createdAt: id,
      type: 'item.completed',
      text: '',
      payload: {
        item: {
          id: 'plan',
          type: 'proposed_plan',
          text: 'Approach',
          request_id: requestId,
        },
      },
    })
    const initial = user(1, 'request-a')
    const completed = proposal(2, 'request-a')
    const clarification = user(3, 'request-b')
    expect(proposedPlanApproval([initial, completed])).toBe('run:2')
    expect(proposedPlanApproval([initial, completed, clarification, proposal(4, 'request-a')])).toBeUndefined()
    expect(proposedPlanApproval([clarification, proposal(4, 'request-a')])).toBeUndefined()
    expect(proposedPlanApproval([proposal(4, null)])).toBeUndefined()
    expect(proposedPlanApproval([clarification, proposal(5, 'request-b')])).toBe('run:5')
    expect(proposedPlanApproval([proposal(5, 'request-b')])).toBe('run:5')
  })
  it('approves completion after steering independently of display anchoring and page loading', () => {
    const started: RunEvent = {
      id: 10,
      runId: 'run',
      createdAt: 10,
      type: 'item.started',
      text: '',
      payload: { item: { id: 'plan', type: 'proposed_plan', text: 'Draft' } },
    }
    const steering: RunEvent = {
      id: 11,
      runId: 'run',
      createdAt: 11,
      type: 'chat.user',
      text: 'Clarification',
    }
    const completed: RunEvent = {
      ...started,
      id: 12,
      createdAt: 12,
      type: 'item.completed',
      payload: { item: { id: 'plan', type: 'proposed_plan', text: 'Clarified plan' } },
    }
    expect(proposedPlanApproval([steering, completed])).toBe('run:12')
    expect(proposedPlanApproval([started, steering, completed])).toBe('run:12')
    expect(proposedPlanApproval([started, steering, completed, { ...steering, id: 13 }])).toBeUndefined()
  })
  it.each([false, true])('keeps subagent proposals attributed when they follow the parent proposal=%s', (childLast) => {
    const event = (id: number, item: Record<string, unknown>): RunEvent => ({
      id,
      runId: 'chat',
      createdAt: id,
      type: 'item.completed',
      text: '',
      payload: { item },
    })
    const parent = { id: 'plan', type: 'proposed_plan', text: 'Parent approach' }
    const child = {
      id: 'plan',
      type: 'proposed_plan',
      parent_id: 'child',
      text: 'Partial child approach',
    }
    const entries = activityEntries([
      event(1, {
        id: 'child',
        type: 'subagent',
        name: 'Research',
        status: 'completed',
      }),
      event(2, childLast ? parent : child),
      event(3, childLast ? child : parent),
    ], true)
    expect(entries.filter(entry => entry.kind === 'proposal')).toMatchObject([{ text: 'Parent approach' }])
    const team = entries.find(entry => entry.kind === 'team')
    expect(team?.kind === 'team' && team.agents[0]?.subagent?.steps).toMatchObject([
      { title: 'Subagent proposed plan', blocks: [{ code: 'Partial child approach' }] },
    ])
    // A paginated window may omit the spawning event; it still must not grant approval.
    const orphan = activityEntries([event(4, child)], true)
    expect(orphan.some(entry => entry.kind === 'proposal')).toBe(false)
    expect(JSON.stringify(orphan)).toContain('Partial child approach')
  })

  it('shows a message at the time the person sent it', () => {
    const card = codex.entries.find(entry => entry.content.kind === 'userMessage')!
    const message = feed(codex, true)[0]!
    expect(message).toMatchObject({ kind: 'message', role: 'user', time: card.content.kind === 'userMessage' ? card.content.sentAt : 0 })
  })

  it('groups actions between assistant messages and updates commands in place', () => {
    const entries = feed(sample, false)
    // The plan shows apart from the tools.
    expect(entries.map(entry => entry.kind)).toEqual(['message', 'plan', 'group', 'message', 'group', 'message', 'wait', 'wait'])
    const groups = entries.filter(entry => entry.kind === 'group')
    expect(groups[0].artifacts.map(item => item.kind)).toEqual(['search', 'search', 'read', 'read', 'browse', 'command'])
    expect(groups[1].artifacts.map(item => item.kind)).toEqual(['files', 'command', 'tool'])
    expect(groups[1].artifacts[1]).toMatchObject({ status: 'done', subtitle: 'pnpm test -- sibling-functions && pnpm typecheck' })
    expect(groups[1].artifacts[0].blocks[0].language).toBe('diff')
    expect(groups[1].artifacts[2].blocks.map(block => block.label)).toEqual(['Arguments', 'Result'])
  })
  it('shows pending, failed and unknown events without losing details', () => {
    const input = (id: number, item: Record<string, unknown>, type = 'item.completed'): RunEvent => ({
      id,
      runId: 'test',
      createdAt: id,
      type,
      text: '',
      payload: { type, item },
    })
    const entries = activityEntries([
      input(1, {
        id: 'a',
        type: 'command_execution',
        command: 'false',
        status: 'in_progress',
      }, 'item.started'),
      input(2, {
        id: 'a',
        type: 'command_execution',
        command: 'false',
        exit_code: 1,
      }),
      input(3, { id: 'b', type: 'future_tool', answer: 'preserved' }),
      input(4, {
        id: 'c',
        type: 'command_execution',
        command: 'sleep 5',
        status: 'in_progress',
      }, 'item.started'),
    ])
    if (entries[0].kind !== 'group')
      throw new Error('Expected a group')
    expect(entries[0].artifacts.map(item => item.status)).toEqual(['error', 'done', 'running'])
    expect(entries[0].artifacts[1].raw).toContain('preserved')
  })
})

describe('operation cards', () => {
  function command(command: string, output = '') {
    const entries = activityEntries([{
      id: 1,
      runId: 'test',
      createdAt: 10,
      type: 'item.completed',
      text: output,
      payload: {
        item: {
          id: 'a',
          type: 'command_execution',
          command,
          aggregated_output: output,
          exit_code: 0,
          status: 'completed',
        },
      },
    }])
    if (entries[0].kind !== 'group')
      throw new Error('Expected an artifact')
    return entries[0].artifacts[0]
  }

  it('recognizes simple shell file reads and highlights the file language', () => {
    const artifact = command('/bin/zsh -lc \'sed -n "1,80p" src/activity.ts\'', 'export const value = 1')
    expect(artifact).toMatchObject({ kind: 'read', title: 'Read activity.ts', subtitle: 'src/activity.ts' })
    expect(artifact.blocks.find(block => block.label === 'File content')?.language).toBe('typescript')
  })
  it('distinguishes repository searches and directory listings from commands', () => {
    expect(command('rg -n "sibling-count" src tests')).toMatchObject({ kind: 'search', title: 'Search files' })
    expect(command('rg --files src')).toMatchObject({ kind: 'browse', title: 'Browse files' })
    expect(command('pnpm test')).toMatchObject({ kind: 'command', title: 'Run tests' })
  })
  it('does not call compound or mutating commands a file read', () => {
    for (const value of ['cat src/a.ts && rm temp', 'cat src/a.ts > copy.ts', 'sed -i s/old/new/ src/a.ts', 'cat $(get-path)', 'cat src/a.ts | sh', 'cat src/[ab].ts', 'cat src/a.ts # comment'])
      expect(command(value).kind).toBe('command')
  })
  it('keeps operation identity and duration across command lifecycle updates', () => {
    const entries = activityEntries([
      {
        id: 1,
        runId: 'test',
        createdAt: 10,
        type: 'item.started',
        text: '',
        payload: {
          item: {
            id: 'read',
            type: 'command_execution',
            command: 'cat src/a.ts',
            status: 'in_progress',
          },
        },
      },
      {
        id: 2,
        runId: 'test',
        createdAt: 250,
        type: 'item.completed',
        text: '',
        payload: {
          item: {
            id: 'read',
            type: 'command_execution',
            command: 'cat src/a.ts',
            aggregated_output: 'const a = 1',
            exit_code: 0,
            status: 'completed',
          },
        },
      },
    ])
    if (entries[0].kind !== 'group')
      throw new Error('Expected an artifact')
    expect(entries[0].artifacts).toHaveLength(1)
    expect(entries[0].artifacts[0]).toMatchObject({ kind: 'read', durationMs: 240, exitCode: 0 })
  })
})

describe('expected command outcomes and connection recovery', () => {
  const event = (id: number, type: string, payload: Record<string, unknown>, text = ''): RunEvent => ({
    id,
    runId: 'test',
    createdAt: id,
    type,
    payload,
    text,
  })
  const artifacts = (events: RunEvent[]) => activityEntries(events).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])
  it('labels search misses and diff results without masking command failures', () => {
    const commands = ['rg needle src', '/bin/bash -lc \'grep needle file\'', 'diff before after', 'git diff --exit-code', 'git diff --quiet', 'rg needle src && pnpm test', 'pnpm test', 'git diff']
    const results = artifacts(commands.map((command, index) => event(index, 'item.completed', {
      item: {
        id: `${index}`,
        type: 'command_execution',
        command,
        exit_code: 1,
        status: 'failed',
      },
    })))
    expect(results.map(item => item.statusLabel)).toEqual(['No matches', 'No matches', 'Differences found', 'Differences found', 'Differences found', undefined, undefined, undefined])
    expect(results.map(item => item.status)).toEqual(['info', 'info', 'info', 'info', 'info', 'error', 'error', 'error'])
    const errors = artifacts([event(1, 'item.completed', { item: { type: 'command_execution', command: 'rg needle missing', exit_code: 2 } }), event(2, 'item.completed', {
      item: {
        type: 'command_execution',
        command: 'rg needle src',
        exit_code: 1,
        error: 'Worker failed',
      },
    })])
    expect(errors.every(item => item.status === 'error')).toBe(true)
  })
})

it.each([false, true])('caps activity groups at thirty tools after folding updates (chat=%s)', (chat) => {
  const events = Array.from({ length: 1001 }, (_, i): RunEvent => ({
    id: i + 1,
    runId: 'r',
    createdAt: i,
    type: 'item.completed',
    text: '',
    payload: {
      item: {
        id: `t${i}`,
        type: 'command_execution',
        command: 'pnpm test',
        exit_code: 0,
      },
    },
  }))
  const entries = activityEntries(events, chat)
  const groups = entries.flatMap(entry => entry.kind === 'group' ? [entry] : [])
  expect(groups).toHaveLength(34)
  expect(groups.every(group => group.artifacts.length <= 30)).toBe(true)
  expect(groups.flatMap(group => group.artifacts).map(item => item.id)).toHaveLength(1001)
  expect(new Set(entries.map(entry => entry.id)).size).toBe(entries.length)
})

it('counts visible tools rather than diagnostic events or gaps in global event IDs', () => {
  const events: RunEvent[] = []
  for (let i = 0; i < 65; i++) {
    const id = 1000 + i * 100
    const item = { id: `tool-${i}`, type: 'command_execution', command: 'echo ok' }
    events.push(
      {
        id,
        runId: 'r',
        createdAt: id,
        type: 'item.started',
        text: '',
        payload: { item: { ...item, status: 'in_progress' } },
      },
      {
        id: id + 1,
        runId: 'r',
        createdAt: id + 1,
        type: 'diagnostic',
        text: 'heartbeat',
        timeline: true,
      },
      {
        id: id + 2,
        runId: 'r',
        createdAt: id + 2,
        type: 'item.updated',
        text: '',
        payload: { item },
      },
      {
        id: id + 3,
        runId: 'r',
        createdAt: id + 3,
        type: 'item.completed',
        text: '',
        payload: { item: { ...item, exit_code: 0 } },
      },
    )
  }

  const groups = activityEntries(events, true).filter(entry => entry.kind === 'group')
  expect(groups.map(group => group.artifacts.length)).toEqual([30, 30, 5])
  expect(groups.flatMap(group => group.artifacts).every(item => item.kind === 'command')).toBe(true)
})

it('also caps tool groups in a subagent timeline', () => {
  const artifact = activityEntries(events).flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])[0]!
  const items = Array.from({ length: 1001 }, (_, i) => ({ ...artifact, kind: 'command' as const, id: `nested-${i}` }))
  const entries = teamEntries([{ kind: 'group', id: 'parent', artifacts: items }])
  expect(entries).toHaveLength(34)
  expect(entries.every(entry => entry.kind === 'group' && entry.artifacts.length <= 30)).toBe(true)
  expect(entries.flatMap(entry => entry.kind === 'group' ? entry.artifacts : [])).toEqual(items)
})

it.each([false, true])('keeps newer tool groups stable when older pages arrive or tools append (chat=%s)', (chat) => {
  const events = Array.from({ length: 191 }, (_, i): RunEvent => ({
    id: i + 1,
    runId: 'r',
    createdAt: i,
    type: 'item.completed',
    text: '',
    payload: {
      item: {
        id: `t${i + 1}`,
        type: 'command_execution',
        command: `echo ${i + 1}`,
        exit_code: 0,
      },
    },
  }))
  const groups = new ActivityGroups()
  const loaded = activityEntries(events.slice(100, 160), chat, groups).filter(entry => entry.kind === 'group')
  const turn: RunEvent = {
    id: 0,
    runId: 'r',
    createdAt: 0,
    type: 'turn.started',
    text: '',
  }
  const extended = activityEntries([turn, ...events], chat, groups).filter(entry => entry.kind === 'group')
  // A full newer group must survive the prepend rather than being repartitioned from index zero.
  expect(extended.find(entry => entry.id === loaded[1]!.id)?.artifacts.map(item => item.command))
    .toEqual(loaded[1]!.artifacts.map(item => item.command))
})

it('preserves full groups through folded older starts and reused tool IDs across turns', () => {
  const tool = (id: number, item: string, type = 'item.completed'): RunEvent => ({
    id,
    runId: 'r',
    createdAt: id,
    type,
    text: '',
    payload: {
      item: {
        id: item,
        type: 'command_execution',
        command: `echo ${id}`,
        exit_code: 0,
      },
    },
  })
  const older = Array.from({ length: 30 }, (_, i) => tool(i + 1, `tool${i}`, 'item.started'))
  const first = Array.from({ length: 30 }, (_, i) => tool(i + 100, `tool${i}`))
  const second = Array.from({ length: 30 }, (_, i) => tool(i + 200, `tool${i - 5}`))
  const turn: RunEvent = {
    id: 199,
    runId: 'r',
    createdAt: 199,
    type: 'turn.started',
    text: '',
  }
  const groups = new ActivityGroups()
  const loaded = activityEntries(first, true, groups).filter(entry => entry.kind === 'group')
  const extended = activityEntries([...older, ...first, turn, ...second], true, groups).filter(entry => entry.kind === 'group')
  expect(extended.map(entry => entry.artifacts.length)).toEqual([30, 30])
  expect(extended[0]!.id).toBe(loaded[0]!.id)
  expect(extended[0]!.artifacts.map(item => item.command)).toEqual(loaded[0]!.artifacts.map(item => item.command))
  expect(activityEntries([...older, ...first, turn, ...second], true, groups)).toEqual(extended)
})

it('keeps newer full groups stable when an older start moves a tool before a notice', () => {
  const tools: RunEvent[] = Array.from({ length: 90 }, (_, i) => ({
    id: i + 3,
    runId: 'r',
    createdAt: i + 3,
    type: 'item.completed',
    text: '',
    payload: {
      item: {
        id: `tool${i}`,
        type: 'command_execution',
        command: `echo ${i}`,
        exit_code: 0,
      },
    },
  }))
  const notice: RunEvent = {
    id: 2,
    runId: 'r',
    createdAt: 2,
    type: 'diagnostic',
    text: 'heartbeat',
  }
  const start = { ...tools[0]!, id: 1, type: 'item.started' }
  const groups = new ActivityGroups()
  const loaded = activityEntries([notice, ...tools], false, groups).filter(entry => entry.kind === 'group')
  const extended = activityEntries([start, notice, ...tools], false, groups).filter(entry => entry.kind === 'group')
  for (const group of loaded.slice(1, 3))
    expect(extended.find(entry => entry.id === group.id)?.artifacts.map(item => item.command)).toEqual(group.artifacts.map(item => item.command))
  expect(new Set(extended.map(entry => entry.id)).size).toBe(extended.length)
})

it('lets partial groups grow while preserving message boundaries', () => {
  const groups = new ActivityGroups()
  const tools = Array.from({ length: 12 }, (_, i): RunEvent => ({
    id: i * 100 + 1,
    runId: 'r',
    createdAt: i,
    type: 'item.completed',
    text: '',
    payload: {
      item: {
        id: `tool-${i}`,
        type: 'command_execution',
        command: 'echo ok',
        exit_code: 0,
      },
    },
  }))
  activityEntries(tools.slice(5, 10), true, groups)
  const extended = activityEntries(tools, true, groups)
  expect(extended).toHaveLength(1)
  expect(extended[0]!.kind === 'group' && extended[0].artifacts).toHaveLength(12)

  const message: RunEvent = {
    id: 1200,
    runId: 'r',
    createdAt: 1200,
    type: 'chat.user',
    text: 'Continue',
    payload: { messageId: 'next-message' },
    timeline: true,
  }
  const card: TimelineEntry = {
    id: 'user:1200',
    runId: 'r',
    position: 1200,
    revision: 1200,
    source: 1200,
    turn: 0,
    attempt: 0,
    createdAt: 1200,
    updatedAt: 1200,
    content: {
      kind: 'userMessage',
      text: 'Continue',
      messageId: 'next-message',
      sentAt: 1200,
      attachments: [],
    },
  }
  const next = {
    ...tools[0]!,
    id: 1300,
    payload: {
      item: {
        id: 'next-tool',
        type: 'command_execution',
        command: 'echo ok',
        exit_code: 0,
      },
    },
  }
  const entries = activityEntries([...tools, message, next], true, groups, undefined, [card])
  expect(entries.map(entry => entry.kind)).toEqual(['group', 'message', 'group'])
})
