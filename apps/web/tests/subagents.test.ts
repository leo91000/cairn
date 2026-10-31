import type { ChatView } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { workingStep } from '../src/signal'
import {
  currentSubagents,
  subagentState,
  subagentSummary,
  subagentTeams,
  waitingForSubagents,
} from '../src/subagents'
import { fixtureEntries } from './timeline-fixture'

let serial = 0

function event(type: string, payload: Record<string, unknown>, createdAt = ++serial * 1000): RunEvent {
  return {
    id: ++serial,
    runId: 'run',
    createdAt,
    type,
    text: '',
    payload,
  }
}

const item = (type: string, value: Record<string, unknown>, createdAt?: number) => event(type, { type, item: value }, createdAt)
const user = (text: string) => event('chat.user', { text, messageId: text })

function subagent(type: string, id: string, values: Record<string, unknown> = {}) {
  return item(type, {
    id,
    type: 'subagent',
    name: id,
    agent_type: 'Explore',
    prompt: `Prompt for ${id}`,
    status: 'in_progress',
    ...values,
  })
}

describe('subagents', () => {
  it('clears the previous report and message when a subagent starts a follow-up', () => {
    const [team] = fixtureEntries([
      subagent('item.started', 'worker'),
      item('item.completed', {
        id: 'reply',
        type: 'subagent_message',
        text: 'Old report',
        parent_id: 'worker',
      }),
      subagent('item.completed', 'worker', { status: 'completed', result: 'Old report' }),
      subagent('item.updated', 'worker', { result: '' }),
    ], true)
    expect(team?.kind === 'team' && team.agents[0]?.subagent).toMatchObject({ state: 'running', result: '', message: '' })
  })

  it('keeps a resumed subagent in its row with its steps and new report', () => {
    const [team] = fixtureEntries([
      subagent('item.started', 'reviewer'),
      item('item.completed', {
        id: 'first',
        type: 'subagent_message',
        text: 'Looks good.',
        parent_id: 'reviewer',
      }),
      subagent('item.completed', 'reviewer', {
        status: 'completed',
        result: 'Looks good.',
        task_id: 'task-review',
        run_id: 'run-1',
      }),
      subagent('item.updated', 'reviewer', { resumed: true, run_id: 'run-2', result: null }),
      item('item.completed', {
        id: 'second',
        type: 'subagent_message',
        text: 'Docs are fine too.',
        parent_id: 'reviewer',
      }),
      subagent('item.updated', 'reviewer', { status: 'completed', result: 'Docs are fine too.', run_id: 'run-2' }),
    ], true)
    expect(team?.kind === 'team' && team.agents).toHaveLength(1)
    expect(team?.kind === 'team' && team.agents[0]?.subagent).toMatchObject({
      state: 'done',
      resumed: true,
      result: 'Docs are fine too.',
      taskId: 'task-review',
    })
  })

  it('folds early lifecycle activity into its spawn without losing metadata or steps', () => {
    const entries = fixtureEntries([
      subagent('item.started', 'spawn'),
      item('item.started', {
        id: 'early',
        type: 'subagent',
        name: 'worker',
        status: 'in_progress',
      }),
      item('item.completed', {
        id: 'read',
        type: 'command_execution',
        command: 'pwd',
        parent_id: 'early',
        status: 'completed',
      }),
      subagent('item.completed', 'spawn', { replaces_id: 'early', name: 'worker' }),
      subagent('item.updated', 'spawn', { replaces_id: 'early', status: 'completed', result: 'Done' }),
    ], true)
    expect(entries).toHaveLength(1)
    const team = entries[0]!
    expect(team.kind === 'team' && team.agents).toHaveLength(1)
    const agent = team.kind === 'team' ? team.agents[0]! : undefined
    expect(agent?.subagent).toMatchObject({ prompt: 'Prompt for spawn', result: 'Done', type: 'Explore' })
    expect(agent?.subagent?.steps.map(step => step.command)).toEqual(['pwd'])
  })

  it('keeps child step identities stable across parent turns and separate between siblings', () => {
    const entries = fixtureEntries([
      subagent('item.started', 'First'),
      subagent('item.started', 'Second'),
      item('item.started', {
        id: 'command',
        type: 'command_execution',
        command: 'pwd',
        parent_id: 'First',
        status: 'in_progress',
      }),
      item('item.completed', {
        id: 'command',
        type: 'command_execution',
        command: 'ls',
        parent_id: 'Second',
        status: 'completed',
      }),
      event('turn.started', { type: 'turn.started' }),
      item('item.completed', {
        id: 'command',
        type: 'command_execution',
        command: 'pwd',
        parent_id: 'First',
        status: 'completed',
        aggregated_output: '/workspace',
      }),
    ], true)
    const team = entries[0]!
    expect(team.kind === 'team' && team.agents.map(agent => agent.subagent?.steps.map(step => [step.command, step.status]))).toEqual([
      [['pwd', 'done']],
      [['ls', 'done']],
    ])
  })

  it('keeps unfinished subagents stopped after another execution starts', () => {
    const entries = fixtureEntries([
      subagent('item.started', 'Previous'),
      item('item.started', {
        id: 'unfinished',
        type: 'command_execution',
        command: 'pnpm test',
        parent_id: 'Previous',
        status: 'in_progress',
      }),
      event('turn.failed', { type: 'turn.failed' }),
      { ...event('turn.started', { type: 'turn.started' }), runId: 'next-run' },
      { ...subagent('item.started', 'Previous'), runId: 'next-run' },
    ], true)
    const agents = entries.flatMap(entry => entry.kind === 'team' ? entry.agents : [])
    expect(agents.map(agent => agent.subagent?.state)).toEqual(['stopped', 'running'])
    expect(agents[0]?.subagent?.steps[0]).toMatchObject({ status: 'info', statusLabel: 'Stopped' })
  })

  it('groups subagents started together and keeps their steps and messages out of the reply', () => {
    const entries = fixtureEntries([
      user('Audit the webhooks'),
      item('item.completed', { id: 'intro', type: 'agent_message', text: 'Starting two subagents.' }),
      subagent('item.started', 'Webhooks'),
      subagent('item.started', 'Tests'),
      item('item.completed', {
        id: 'grep',
        type: 'command_execution',
        command: 'rg -n webhook',
        exit_code: 0,
        parent_id: 'Webhooks',
      }),
      item('item.completed', {
        id: 'note',
        type: 'subagent_message',
        text: 'Found three handlers.',
        parent_id: 'Webhooks',
      }),
      subagent('item.updated', 'Webhooks', { activity: 'Reading webhook.rs' }),
      subagent('item.completed', 'Tests', {
        status: 'completed',
        result: 'All billing tests pass.',
        duration_ms: 4200,
        tokens: 1510,
      }),
      item('item.completed', { id: 'reply', type: 'agent_message', text: 'Waiting for the webhook audit.' }),
    ], true)

    expect(entries.map(entry => entry.kind)).toEqual(['message', 'message', 'team', 'message'])
    const team = entries[2]!
    expect(team.kind === 'team' && team.agents.map(agent => [agent.title, agent.status, agent.subagent?.state])).toEqual([
      ['Webhooks', 'running', 'running'],
      ['Tests', 'done', 'done'],
    ])
    const [webhooks, tests] = team.kind === 'team' ? team.agents : []
    expect(webhooks!.subagent).toMatchObject({
      type: 'Explore',
      prompt: 'Prompt for Webhooks',
      activity: 'Reading webhook.rs',
      message: 'Found three handlers.',
    })
    expect(webhooks!.subagent!.steps.map(step => [step.kind, step.command])).toEqual([['search', 'rg -n webhook']])
    expect(tests!.subagent).toMatchObject({ result: 'All billing tests pass.', tokens: 1510 })
    expect(tests!.durationMs).toBe(4200)
    expect(entries.filter(entry => entry.kind === 'message').map(entry => entry.text)).not.toContain('Found three handlers.')
  })

  it('keeps the agent steps around a team in their order', () => {
    const entries = fixtureEntries([
      item('item.completed', {
        id: 'read',
        type: 'command_execution',
        command: 'cat README.md',
        exit_code: 0,
      }),
      subagent('item.started', 'Review'),
      item('item.completed', {
        id: 'test',
        type: 'command_execution',
        command: 'pnpm test',
        exit_code: 0,
      }),
    ], true)
    expect(entries.map(entry => entry.kind === 'group' ? entry.artifacts.map(artifact => artifact.command) : entry.kind)).toEqual([
      ['cat README.md'],
      'team',
      ['pnpm test'],
    ])
  })

  it('names Codex subagent tools and keeps orphaned messages readable', () => {
    const [group] = fixtureEntries([
      item('item.completed', {
        id: 'close',
        type: 'collab_tool_call',
        tool: 'closeAgent',
        prompt: null,
        status: 'completed',
      }),
      item('item.completed', {
        id: 'lost',
        type: 'subagent_message',
        text: 'From an unloaded subagent',
        parent_id: 'missing',
      }),
    ])
    expect(group?.kind === 'group' && group.artifacts.map(artifact => artifact.title)).toEqual(['Closed a subagent', 'Subagent message'])
  })

  it('keeps one row per subagent across turns', () => {
    const [team] = fixtureEntries([
      subagent('item.completed', 'Background', { background: true }),
      event('turn.started', { type: 'turn.started' }),
      subagent('item.updated', 'Background', { background: true, status: 'completed', result: 'Done later' }),
    ], true)
    expect(team?.kind === 'team' && team.agents.map(agent => [agent.title, agent.subagent?.result])).toEqual([['Background', 'Done later']])
  })

  it('marks failed and stopped subagents', () => {
    const [team] = fixtureEntries([
      subagent('item.completed', 'Broken', { status: 'failed', result: 'Permission denied' }),
      subagent('item.completed', 'Closed', { status: 'stopped' }),
    ])
    expect(team?.kind === 'team' && team.agents.map(agent => [agent.status, agent.statusLabel, agent.subagent?.state])).toEqual([
      ['error', undefined, 'failed'],
      ['done', 'Stopped', 'stopped'],
    ])
  })

  it('tells that the agent waits for its subagents', () => {
    const events = [
      user('Audit'),
      subagent('item.started', 'Webhooks'),
      subagent('item.started', 'Tests'),
    ]
    expect(workingStep(fixtureEntries(events, true), 'Claude', null)).toMatchObject({ title: 'Waiting for 2 subagents', detail: 'Webhooks · Tests', subagents: true })
    const one = [...events, subagent('item.completed', 'Tests', { status: 'completed' })]
    expect(workingStep(fixtureEntries(one, true), 'Claude', null)).toMatchObject({ title: 'Waiting for a subagent', detail: 'Webhooks' })
    const acting = [...one, item('item.started', {
      id: 'lint',
      type: 'command_execution',
      command: 'pnpm lint',
      status: 'in_progress',
    })]
    expect(workingStep(fixtureEntries(acting, true), 'Claude', null).title).not.toContain('subagent')
  })

  it('finds the subagents of the current request and what each one does', () => {
    const entries = fixtureEntries([
      user('First'),
      subagent('item.completed', 'Earlier', { status: 'completed', result: '## Findings\nAll good' }),
      user('Second'),
      subagent('item.started', 'Webhooks', { activity: 'Reading webhook.rs' }),
      item('item.completed', { id: 'note', type: 'agent_message', text: 'Meanwhile…' }),
      subagent('item.started', 'Tests'),
    ], true)
    const current = currentSubagents(entries)
    expect(current.map(agent => agent.title)).toEqual(['Webhooks', 'Tests'])
    expect(current.map(agent => subagentSummary(agent, true))).toEqual(['Reading webhook.rs', 'Starting…'])
    // Once the run ends, a subagent that never reported shows as stopped.
    expect(subagentState(current[0]!, false)).toBe('stopped')
    const earlier = fixtureEntries([subagent('item.completed', 'Earlier', { status: 'completed', result: '## Findings\nAll good' })])
    expect(earlier[0]?.kind === 'team' && subagentSummary(earlier[0].agents[0]!, false)).toBe('Findings')
  })

  it('keeps earlier workers and nested children reachable after another user message', () => {
    const entries = fixtureEntries([
      user('First'),
      subagent('item.started', 'Earlier'),
      subagent('item.completed', 'Done', { status: 'completed' }),
      subagent('item.started', 'Nested', { parent_id: 'Earlier' }),
      user('Keep going'),
      subagent('item.started', 'New'),
    ], true)
    expect(currentSubagents(entries).map(agent => agent.title)).toEqual(['Earlier', 'Nested', 'New'])
    expect(subagentTeams(entries).map(team => team.map(agent => agent.title))).toEqual([
      ['Earlier', 'Done'],
      ['Nested'],
      ['New'],
    ])
  })

  it('replaces an explicit wait only when every awaited task is represented in the bar', () => {
    const events = [
      subagent('item.completed', 'Worker', { background: true, task_id: 'worker-task' }),
      event('turn.waiting', { type: 'turn.waiting', tasks: [{ id: 'worker-task', description: 'Worker' }] }),
    ]
    const agents = currentSubagents(fixtureEntries(events, true))
    expect(waitingForSubagents(['worker-task'], agents)).toBe(true)
    expect(waitingForSubagents(['worker-task', 'build-task'], agents)).toBe(false)
    expect(waitingForSubagents([], agents)).toBe(false)
  })
})

describe('delegated tasks', () => {
  const delegated = (chatId: string) => event('chat.delegated', {
    chatId,
    title: `Task ${chatId}`,
    agentId: 'main',
    agentName: 'Reviewer',
    provider: 'codex',
    model: 'gpt-5.5',
    task: `Review for ${chatId}`,
  })
  const toolCall = item('item.completed', {
    id: 'call',
    type: 'mcp_tool_call',
    server: 'cairn_workspace',
    tool: 'delegate_task',
    status: 'completed',
  })
  const view = (id: string, values: Partial<ChatView> = {}): ChatView => ({
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
    runId: 'child-run',
    paused: false,
    createdAt: 1,
    updatedAt: 1,
    pendingQuestions: 0,
    agentName: 'Reviewer',
    projectName: null,
    status: 'running',
    ...values,
  })

  it('shows tasks delegated together as one team instead of their tool calls', () => {
    const entries = fixtureEntries([user('Review it'), toolCall, delegated('a'), delegated('b'), delegated('a')], true)
    const team = entries.find(entry => entry.kind === 'team')
    expect(team?.kind === 'team' && team.agents.map(agent => agent.subagent?.delegation)).toEqual([
      { chatId: 'a', agentName: 'Reviewer', provider: 'codex' },
      { chatId: 'b', agentName: 'Reviewer', provider: 'codex' },
    ])
    expect(team?.kind === 'team' && team.agents[0]).toMatchObject({ title: 'Task a', subagent: { prompt: 'Review for a', model: 'gpt-5.5' } })
    expect(entries.some(entry => entry.kind === 'group' && entry.artifacts.some(artifact => artifact.kind === 'tool'))).toBe(false)
  })

  it('follows each delegated task through its conversation', () => {
    const [agent] = currentSubagents(fixtureEntries([user('Review it'), delegated('a')], true))
    const state = (chat?: ChatView) => {
      const chats = new Map(chat ? [[chat.id, chat]] : [])
      return [subagentState(agent!, false, chats), subagentSummary(agent!, false, chats)]
    }

    expect(state(view('a'))).toEqual(['running', 'Working'])
    expect(state(view('a', { runId: null, status: 'queued' }))).toEqual(['running', 'Waiting for a runner'])
    expect(state(view('a', { runId: null, status: 'idle' }))).toEqual(['stopped', 'Stopped'])
    expect(state(view('a', { status: 'succeeded', pendingQuestions: 1 }))).toEqual(['running', 'Asks a question'])
    expect(state(view('a', { status: 'succeeded', preview: '2 findings' }))).toEqual(['done', '2 findings'])
    expect(state(view('a', { status: 'succeeded' }))).toEqual(['done', 'Reported back'])
    expect(state(view('a', { status: 'cancelled', paused: true }))).toEqual(['stopped', 'Stopped'])
    expect(state(view('a', { status: 'succeeded', paused: true, pendingQuestions: 1 }))).toEqual(['stopped', 'Stopped'])
    expect(state(view('a', { status: 'running', paused: true, pendingQuestions: 2 }))).toEqual(['stopped', 'Stopped'])
    expect(state(view('a', { status: 'failed' }))).toEqual(['failed', 'Failed'])
    expect(state()).toEqual(['stopped', 'Conversation deleted'])
  })

  it('keeps earlier delegated tasks with the request that started them', () => {
    const entries = fixtureEntries([user('First'), delegated('a'), user('Second')], true)
    expect(currentSubagents(entries)).toEqual([])
  })
})
