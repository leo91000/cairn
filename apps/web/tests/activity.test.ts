import type { RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import sample from '../../../tests/fixtures/activity-events.json'
import { redactPayload } from '../../../tests/fixtures/legacy/server/worker.ts'
import { activityEntries, ActivityGroups, teamEntries } from '../src/activity'

const events: RunEvent[] = sample.slice(0, 15).map((payload, index) => ({
  id: index + 1,
  runId: 'test',
  createdAt: index,
  type: payload.type,
  text: '',
  payload,
}))
describe('conversation activity', () => {
  it('labels each reply with the provider its attempt used, until another attempt begins', () => {
    let id = 0
    const event = (type: string, text: string, payload?: Record<string, unknown>): RunEvent => ({
      id: ++id,
      runId: 'chat',
      createdAt: id,
      type,
      text,
      ...(payload ? { payload } : {}),
    })
    const reply = (text: string) => event('item.completed', '', { item: { id: `m${id}`, type: 'agent_message', text } })
    const entries = activityEntries([
      event('status', 'Preparing workspace', { attempt: true }),
      event('status', 'Kimi K2 via OpenCode Go', { route: { model: 'opencode-go/kimi-k2', providerName: 'OpenCode Go' } }),
      event('status', 'Opening project'),
      reply('First'),
      event('status', 'Preparing workspace', { attempt: true }),
      event('status', 'Kimi K2 via OpenCode Zen', { route: { model: 'opencode/kimi-k2', providerName: 'OpenCode Zen' } }),
      reply('Second'),
      event('status', 'Preparing workspace', { attempt: true }),
      reply('Third'),
    ], true)
    const replies = entries.filter(entry => entry.kind === 'message').map(entry => [entry.text, entry.via])
    expect(replies).toEqual([['First', 'OpenCode Go'], ['Second', 'OpenCode Zen'], ['Third', undefined]])
    expect(entries.some(entry => entry.kind !== 'message' && JSON.stringify(entry).includes('via OpenCode'))).toBe(false)
  })

  it('hides routine session updates in chat while preserving the raw activity and tool steps', () => {
    const input: RunEvent[] = [
      {
        id: 1,
        runId: 'chat',
        createdAt: 1,
        type: 'turn.started',
        text: '',
      },
      {
        id: 2,
        runId: 'chat',
        createdAt: 2,
        type: 'item.completed',
        text: '',
        payload: {
          item: {
            id: 'tool',
            type: 'command_execution',
            command: 'pnpm test',
            exit_code: 0,
          },
        },
      },
      {
        id: 3,
        runId: 'chat',
        createdAt: 3,
        type: 'diagnostic',
        text: 'Worker ready',
      },
      {
        id: 4,
        runId: 'chat',
        createdAt: 4,
        type: 'item.completed',
        text: 'The checks passed.',
      },
      {
        id: 5,
        runId: 'chat',
        createdAt: 5,
        type: 'turn.completed',
        text: '',
      },
      {
        id: 6,
        runId: 'chat',
        createdAt: 6,
        type: 'status',
        text: 'succeeded',
      },
    ]
    const raw = activityEntries(input)
    const chat = activityEntries(input, true)
    expect(chat.map(entry => entry.kind)).toEqual(['group', 'message'])
    expect(chat[0]).toMatchObject({ artifacts: [{ kind: 'command', command: 'pnpm test' }] })
    expect(chat[0].kind === 'group' && chat[0].artifacts).toHaveLength(1)
    expect(raw.flatMap(entry => entry.kind === 'group' ? entry.artifacts : []).filter(item => item.kind === 'notice')).toHaveLength(4)
    expect(activityEntries(input)).toEqual(raw)
  })
  it('keeps failures and interruptions inline in chat, in order with real tool steps', () => {
    const input: RunEvent[] = [
      {
        id: 1,
        runId: 'chat',
        createdAt: 1,
        type: 'status',
        text: 'running',
      },
      {
        id: 2,
        runId: 'chat',
        createdAt: 2,
        type: 'item.completed',
        text: '',
        payload: {
          item: {
            id: 'a',
            type: 'command_execution',
            command: 'pnpm test',
            exit_code: 1,
          },
        },
      },
      {
        id: 3,
        runId: 'chat',
        createdAt: 3,
        type: 'error',
        text: '',
        payload: { message: 'Validation failed' },
      },
      {
        id: 4,
        runId: 'chat',
        createdAt: 4,
        type: 'item.started',
        text: '',
        payload: {
          item: {
            id: 'b',
            type: 'command_execution',
            command: 'pnpm build',
            status: 'in_progress',
          },
        },
      },
      {
        id: 5,
        runId: 'chat',
        createdAt: 5,
        type: 'status',
        text: 'interrupted',
      },
    ]
    const chat = activityEntries(input, true)
    expect(chat.map(entry => entry.kind)).toEqual(['group', 'notice', 'group', 'notice'])
    expect(chat[0]).toMatchObject({ artifacts: [{ status: 'error' }] })
    expect(chat[1]).toMatchObject({ artifact: { blocks: [{ code: 'Validation failed' }] } })
    expect(chat[2]).toMatchObject({ artifacts: [{ status: 'running' }] })
    expect(chat[3]).toMatchObject({ artifact: { title: 'Interrupted' } })
    for (const text of ['failed', 'cancelled', 'interrupted'])
      expect(activityEntries([{ ...input[4], text }], true)).toHaveLength(1)
  })
  it('hides recovered connection retries but keeps unresolved ones visible in chat', () => {
    const connection: RunEvent = {
      id: 1,
      runId: 'chat',
      createdAt: 1,
      type: 'diagnostic',
      text: 'Reconnecting... 2/5',
    }
    expect(activityEntries([connection], true)[0]).toMatchObject({ kind: 'notice', artifact: { title: 'Connection interrupted' } })
    expect(activityEntries([connection, {
      id: 2,
      runId: 'chat',
      createdAt: 2,
      type: 'turn.completed',
      text: '',
    }], true)).toEqual([])
    expect(activityEntries([{ ...connection, type: 'thread.started', text: '' }], true)).toEqual([])
  })
  it('shows the message submission time with a fallback for older events', () => {
    const message = {
      id: 1,
      runId: 'test',
      createdAt: 60000,
      type: 'chat.user',
      text: 'Hello',
    }
    expect(activityEntries([{ ...message, payload: { createdAt: 1000 } }])[0]).toMatchObject({ time: 1000, role: 'user' })
    for (const createdAt of [undefined, null, 'invalid', -1, Number.NaN, Number.MAX_VALUE])
      expect(activityEntries([{ ...message, payload: { createdAt } }])[0]).toMatchObject({ time: 60000 })
  })
  it('groups actions between assistant messages and updates commands in place', () => {
    const entries = activityEntries(events)
    // The plan shows apart from the tools.
    expect(entries.map(entry => entry.kind)).toEqual(['message', 'plan', 'group', 'message', 'group', 'message'])
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
  it('keeps historical plain output readable and pairs legacy command events', () => {
    const old = (id: number, type: string, text: string): RunEvent => ({
      id,
      type,
      text,
      runId: 'old',
      createdAt: id,
    })
    const entries = activityEntries([old(1, 'item.completed', 'I will check this.'), old(2, 'item.started', ''), old(3, 'item.completed', '8 tests passed'), old(4, 'item.completed', 'The checks passed.')])
    expect(entries.map(entry => entry.kind)).toEqual(['message', 'group', 'message'])
    if (entries[1].kind !== 'group')
      throw new Error('Expected a group')
    expect(entries[1].artifacts[0].blocks[0].code).toBe('8 tests passed')
  })
  it('redacts structured tool arguments and nested text before persistence', () => {
    expect(redactPayload({ arguments: { access_token: 'private-value' }, result: { text: 'Bearer private-token' } })).toEqual({ arguments: { access_token: '[redacted]' }, result: { text: 'Bearer [redacted]' } })
  })
})

describe('artifact identities', () => {
  it('summarizes historical JSON without copying it into the card heading', () => {
    const output = JSON.stringify([{ id: 123, jobs: [{ name: 'quality', conclusion: 'success' }] }])
    const entries = activityEntries([
      {
        id: 1,
        runId: 'old',
        createdAt: 10,
        type: 'item.started',
        text: '',
      },
      {
        id: 2,
        runId: 'old',
        createdAt: 20,
        type: 'item.completed',
        text: output,
      },
    ])
    if (entries[0].kind !== 'group')
      throw new Error('Expected an artifact')
    expect(entries[0].artifacts[0]).toMatchObject({ title: 'Workflow checks', subtitle: '1 item', historical: true })
    expect(entries[0].artifacts[0].raw).toBe(output)
  })
  it('keeps historical JSON output with its step instead of losing the pairing', () => {
    const entries = activityEntries([
      {
        id: 1,
        runId: 'old',
        createdAt: 10,
        type: 'item.started',
        text: '',
      },
      {
        id: 2,
        runId: 'old',
        createdAt: 20,
        type: 'item.completed',
        text: '{"files":["src/a.ts"]}',
      },
    ])
    expect(entries).toHaveLength(1)
    if (entries[0].kind !== 'group')
      throw new Error('Expected an artifact')
    expect(entries[0].artifacts).toHaveLength(1)
    expect(entries[0].artifacts[0]).toMatchObject({
      title: 'Structured result',
      subtitle: '1 field',
      status: 'info',
      blocks: [{ label: 'Output', code: '{"files":["src/a.ts"]}', language: 'plaintext' }],
    })
  })
  it('labels historical output honestly instead of leaving a completed Working card', () => {
    const entries = activityEntries([
      {
        id: 1,
        runId: 'old',
        createdAt: 10,
        type: 'item.started',
        text: '',
      },
      {
        id: 2,
        runId: 'old',
        createdAt: 20,
        type: 'item.completed',
        text: 'Already up to date.\ncompatibility/css-feature-target.json',
      },
    ])
    if (entries[0].kind !== 'group')
      throw new Error('Expected an artifact')
    expect(entries[0].artifacts[0]).toMatchObject({
      kind: 'output',
      title: 'Recorded output',
      historical: true,
      status: 'info',
    })
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
  it('marks temporary connection trouble recovered only after subsequent progress', () => {
    const connection = event(1, 'error', { message: 'WebSocket connection failed: 503 Service Unavailable' })
    const unrelated = event(2, 'error', { message: 'Validation failed' })
    expect(artifacts([connection])[0].status).toBe('error')
    const results = artifacts([connection, unrelated, event(3, 'item.completed', { item: { type: 'agent_message', text: 'Continuing the audit.' } })])
    expect(results[0]).toMatchObject({ status: 'info', statusLabel: 'Recovered', title: 'Connection interrupted' })
    expect(results[0].raw).toContain('503')
    expect(results[1].status).toBe('error')
    expect(artifacts([connection, event(3, 'turn.failed', {}), event(4, 'turn.started', {}), event(5, 'turn.completed', {})])[0].status).toBe('error')
  })
  it('recognizes historical reconnect diagnostics and preserves the original detail', () => {
    const old = {
      id: 1,
      runId: 'old',
      createdAt: 1,
      type: 'diagnostic',
      text: 'Reconnecting... 2/5',
    }
    const result = artifacts([old, event(2, 'turn.completed', {})])[0]
    expect(result).toMatchObject({ status: 'info', statusLabel: 'Recovered', raw: old.text })
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
  const entries = activityEntries([...tools, message, next], true, groups)
  expect(entries.map(entry => entry.kind)).toEqual(['group', 'message', 'group'])
})
