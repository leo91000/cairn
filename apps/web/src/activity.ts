import type { ChatWakeup } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import type { AgentPlan, PlanChange, PlanProgress } from './agent-plan'
import { describeCommand, expectedCommandOutcome } from './activity-command'
import { outputSummary } from './activity-data'
import {
  continuesPlan,
  planChange,
  planProgress,
  planSteps,
  samePlan,
} from './agent-plan'

export const MAX_ACTIVITY_GROUP_SIZE = 30

export interface ActivityCode {
  label: string
  code: string
  language: string
}
/** A subagent the agent delegated work to, with the steps it took on its own. */
export interface ActivitySubagent {
  taskId?: string
  type: string
  prompt: string
  /** Its final report, once it finished. */
  result: string
  /** Its latest message while it works. */
  message: string
  /** What Claude Code says it is doing. */
  activity: string
  model: string
  background: boolean
  state: 'running' | 'done' | 'failed' | 'stopped'
  tokens?: number
  toolUses?: number
  steps: ActivityArtifact[]
}
export interface ActivityArtifact {
  kind: 'command' | 'read' | 'browse' | 'output' | 'files' | 'search' | 'tool' | 'plan' | 'thinking' | 'notice' | 'subagent'
  id: string
  /** Persisted events folded into this activity, independent of loaded turn boundaries. */
  sourceIds?: string[]
  time: number
  title: string
  subtitle: string
  status: 'running' | 'done' | 'error' | 'info'
  blocks: ActivityCode[]
  files: { path: string, kind: string }[]
  tasks: { text: string, completed: boolean }[]
  raw: string
  command?: string
  cwd?: string
  exitCode?: number
  durationMs?: number
  historical?: boolean
  statusLabel?: string
  attention?: boolean
  subagent?: ActivitySubagent
}
export type ActivityEntry
  = | {
    kind: 'message'
    role?: 'user'
    id: string
    time: number
    text: string
    delivery?: string
    /** The model provider that wrote this reply, when the engine chose among several. */
    via?: string
    attachments?: import('../../../packages/contracts/chats').ChatAttachment[]
  }
  | { kind: 'group', id: string, artifacts: ActivityArtifact[] }
  | { kind: 'team', id: string, agents: ActivityArtifact[] }
  | { kind: 'notice', id: string, artifact: ActivityArtifact }
  /** The agent's plan, kept up to date by its later updates. */
  | { kind: 'plan', id: string, plan: AgentPlan }
  /** Where an update changed the plan, with its progress afterwards. */
  | {
    kind: 'plan-update'
    id: string
    planId: string
    time: number
    change: PlanChange
    progress: PlanProgress
  }
  | {
    kind: 'wait'
    id: string
    time: number
    wait: ActivityWait
  }

/** A durable wait the agent scheduled with `sleep` or `wait_for`, then how it ended. */
export interface ActivityWait {
  /** Absent when the condition had already completed and the agent never waited. */
  wakeup: ChatWakeup | null
  /** What Cairn observed once the condition completed or the deadline passed. */
  observation?: Record<string, unknown> | null
  /** When the conversation resumed. */
  resumedAt?: number
  /** A message from the user or a failed turn cancelled it, or another wait replaced it. */
  ended?: 'cancelled' | 'replaced'
  /** The agent's reason, for a result that was already available: no wakeup carries it. */
  reason?: string
}

/** Historical marker for events that predate the explicit immediate boolean. */
const immediateResult = 'Wait condition already completed'
// Where a scheduled wait's card is placed: its turn ended, or a message or another wait event came.
const waitBoundaries = new Set(['turn.started', 'turn.completed', 'turn.failed', 'chat.user', 'chat.sleep', 'chat.wait', 'chat.wait_ready', 'chat.wakeup'])
const terminalStatuses = new Set(['succeeded', 'failed', 'cancelled', 'interrupted'])

// Codex names the workspace server; Claude and OpenCode prefix the tool with it.
const workspaceWaitTool = /^(?:mcp__)?cairn_workspace(?:__|_|\.)(?:sleep|wait_for)$/

/** A tool call's arguments, which some providers record as JSON text. */
function toolArguments(value: unknown) {
  if (typeof value !== 'string')
    return record(value)
  try {
    return record(JSON.parse(value))
  }
  catch {
    return {}
  }
}

function isWaitTool(item: Record<string, unknown>) {
  const tool = text(item.tool)
  return workspaceWaitTool.test(tool) || (text(item.server) === 'cairn_workspace' && ['sleep', 'wait_for'].includes(tool))
}

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : {}
}

function text(value: unknown) {
  return typeof value === 'string' ? value : ''
}

function pretty(value: unknown) {
  return typeof value === 'string' ? value : JSON.stringify(value, null, 2) ?? ''
}

function payload(event: RunEvent) {
  if (event.payload)
    return event.payload
  try {
    const data = record(JSON.parse(event.text))
    return event.type === 'chat.wait_ready' || data.type === event.type || Object.hasOwn(data, 'item') ? data : {}
  }
  catch {
    return {}
  }
}

function readable(value: string) {
  return value.replaceAll(/[._]/g, ' ').replace(/^./, letter => letter.toUpperCase())
}

const subagentStates: Record<string, ActivitySubagent['state']> = {
  completed: 'done',
  failed: 'failed',
  stopped: 'stopped',
}
// Codex's other subagent tools; spawning, waiting and listing are folded into the subagents.
const collaborationTitles: Record<string, string> = {
  sendInput: 'Sent instructions to a subagent',
  sendMessage: 'Messaged a subagent',
  followupTask: 'Gave a subagent a follow-up task',
  resumeAgent: 'Resumed a subagent',
  interruptAgent: 'Interrupted a subagent',
  closeAgent: 'Closed a subagent',
}

/** Keep tool updates in their original position and fold actions between messages. */
export function activityEntries(events: RunEvent[], chat = false, groups = new ActivityGroups(), pendingWakeup?: ChatWakeup | null): ActivityEntry[] {
  const entries: ActivityEntry[] = []
  const artifacts = new Map<string, ActivityArtifact>()
  const messages = new Map<string, Extract<ActivityEntry, { kind: 'message' }>>()
  // By the agent's own item ID: a subagent's steps can arrive after the turn that started it.
  const subagents = new Map<string, ActivityArtifact>()
  const aliases = new Map<string, string>()
  for (const event of events) {
    const item = record(payload(event).item)
    if (item.type === 'subagent' && text(item.replaces_id))
      aliases.set(`${event.runId}:${text(item.replaces_id)}`, text(item.id))
  }

  const agentKey = (event: RunEvent, id: string) => `${event.runId}:${aliases.get(`${event.runId}:${id}`) ?? id}`
  let connectionNotices: ActivityArtifact[] = []

  function recovered() {
    for (const notice of connectionNotices) {
      notice.status = 'info'
      notice.statusLabel = 'Recovered'
      notice.subtitle = 'Connection restored; work continued'
      notice.attention = false
    }

    connectionNotices = []
  }

  let turn = 0
  let via: string | undefined
  // The plan its updates change, until the agent finishes it or writes another one.
  let plan: Extract<ActivityEntry, { kind: 'plan' }> | undefined
  let legacyTool: ActivityArtifact | undefined
  // The wait still pending, by its wakeup ID, so that its result and resumption complete it.
  const waits = new Map<string, ActivityWait>()
  let openWait: ActivityWait | undefined
  // The current wait call's reason: its tool row is hidden, and an immediate result omits it.
  // Only that call may lend it, so it resets once used, once a wakeup carries it, and each turn.
  let waitCall = ''
  let waitReason = ''
  // The call whose reason was used: its later updates must not lend it again.
  let spentCall = ''
  // A scheduled wait only starts once its turn ends: its card follows what the agent did after
  // the call, and is placed when the turn ends, another wait or a message comes, or the
  // history does.
  let scheduled: Extract<ActivityEntry, { kind: 'wait' }> | undefined
  const placeScheduled = () => {
    if (scheduled)
      entries.push(scheduled)
    scheduled = undefined
  }

  for (const [eventIndex, event] of events.entries()) {
    if (event.type === 'artifact')
      continue
    if (waitBoundaries.has(event.type) || (event.type === 'status' && terminalStatuses.has(event.text)))
      placeScheduled()
    if (event.type === 'chat.sleep' || event.type === 'chat.wait') {
      const wakeup = record(event.payload) as unknown as ChatWakeup
      if (!wakeup.id || waits.has(wakeup.id))
        continue
      if (openWait)
        openWait.ended = 'replaced'
      openWait = { wakeup }
      waits.set(wakeup.id, openWait)
      waitReason = ''
      spentCall = waitCall
      scheduled = {
        kind: 'wait',
        id: `wait:${wakeup.id}`,
        time: event.createdAt,
        wait: openWait,
      }
      continue
    }

    // The condition completed right away: the agent never waited, and this call replaced any wait.
    const observation = event.type === 'chat.wait_ready' ? payload(event) : {}
    const immediate = typeof observation.immediate === 'boolean' ? observation.immediate : event.text === immediateResult
    if (event.type === 'chat.wait_ready' && immediate) {
      if (openWait) {
        openWait.ended = 'replaced'
        openWait = undefined
      }

      // Cairn records the reason with the result; older histories only kept it on the call.
      const reason = text(observation.reason) || waitReason
      entries.push({
        kind: 'wait',
        id: `wait-ready:${event.id}`,
        time: event.createdAt,
        wait: { wakeup: null, observation, ...(reason ? { reason } : {}) },
      })
      waitReason = ''
      spentCall = waitCall
      continue
    }

    // Polling found the result while the conversation waited. When the scheduling is outside the
    // loaded page, a later resumption or the live snapshot identifies the wait.
    if (event.type === 'chat.wait_ready') {
      const following = events.slice(eventIndex + 1)
      const boundary = following.findIndex(next => ['chat.user', 'chat.wait', 'chat.sleep'].includes(next.type))
      const continuation = boundary < 0 ? following : following.slice(0, boundary)
      const resumed = continuation.find(next => next.type === 'chat.wakeup'
        && JSON.stringify(next.payload?.observation) === JSON.stringify(observation))
      const open = openWait?.wakeup?.condition ? openWait.wakeup : undefined
      const legacy = resumed?.payload as unknown as ChatWakeup | undefined
      const pending = pendingWakeup?.condition && pendingWakeup.createdAt <= event.createdAt ? pendingWakeup : undefined
      const target = open ?? legacy ?? pending
      let wait = target?.id ? waits.get(target.id) : undefined
      if (!wait && target?.id) {
        wait = { wakeup: target }
        waits.set(target.id, wait)
        entries.push({
          kind: 'wait',
          id: `wait:${target.id}`,
          time: event.createdAt,
          wait,
        })
      }

      if (wait) {
        wait.observation = observation
      }
      else {
        entries.push({
          kind: 'wait',
          id: `wait-ready:${event.id}`,
          time: event.createdAt,
          wait: { wakeup: null, observation },
        })
      }

      continue
    }

    if (event.type === 'chat.wakeup') {
      const wakeup = record(event.payload) as unknown as ChatWakeup
      let wait = wakeup.id ? waits.get(wakeup.id) : undefined
      // Its scheduling may be in an older page of the history.
      if (!wait) {
        wait = { wakeup }
        if (wakeup.id)
          waits.set(wakeup.id, wait)
        entries.push({
          kind: 'wait',
          id: `wait:${wakeup.id || event.id}`,
          time: event.createdAt,
          wait,
        })
      }

      wait.observation = wakeup.observation ?? wait.observation ?? null
      wait.resumedAt = event.createdAt
      if (wait === openWait)
        openWait = undefined
      continue
    }

    if (event.type === 'chat.user') {
      // A new request gets its own plan, even when the agent carries the previous list over.
      plan = undefined
      // A new message cancels the pending wakeup.
      if (openWait) {
        openWait.ended = 'cancelled'
        openWait = undefined
      }

      entries.push({
        kind: 'message',
        role: 'user',
        id: `user:${event.id}`,
        time: typeof event.payload?.createdAt === 'number' && Number.isFinite(event.payload.createdAt) && event.payload.createdAt > 0 && event.payload.createdAt <= event.createdAt ? event.payload.createdAt : event.createdAt,
        text: typeof event.payload?.text === 'string' ? event.payload.text : event.text,
        attachments: Array.isArray(event.payload?.attachments) ? event.payload.attachments : [],
      })
      continue
    }

    const data = payload(event)
    const route = record(data.route)
    // An attempt may name the provider of its replies; the next attempt ends that label.
    if (event.type === 'status' && data.attempt === true)
      via = undefined
    if (event.type === 'status' && text(route.providerName)) {
      via = text(route.providerName)
      if (chat)
        continue
    }

    const item = record(data.item)
    const type = text(item.type)
    // A run that ends without succeeding never resumes the wait it scheduled. Only its terminal
    // status says so: a failed turn may be retried, and the retry keeps the wakeup.
    if (openWait && !openWait.observation && event.type === 'status' && ['failed', 'cancelled', 'interrupted'].includes(event.text)) {
      openWait.ended = 'cancelled'
      openWait = undefined
    }

    // Keep ended children stopped in the history when a later execution starts.
    const ended = ['turn.completed', 'turn.failed'].includes(event.type) || (event.type === 'status' && ['succeeded', 'failed', 'cancelled', 'interrupted'].includes(event.text))
    if (ended) {
      for (const [key, agent] of subagents) {
        if (key.startsWith(`${event.runId}:`) && agent.subagent?.state === 'running') {
          agent.subagent.state = 'stopped'
          agent.status = 'done'
          agent.statusLabel = 'Stopped'
          agent.durationMs = Math.max(0, event.createdAt - agent.time)
          for (const step of agent.subagent.steps) {
            if (step.status === 'running' && !step.subagent) {
              step.status = 'info'
              step.statusLabel = 'Stopped'
              step.durationMs = Math.max(0, event.createdAt - step.time)
            }
          }
        }
      }
    }

    if (event.type === 'turn.started') {
      // Item IDs may repeat in a later turn: a new turn's calls start afresh.
      waitReason = ''
      waitCall = ''
      spentCall = ''
      connectionNotices = []
      turn++
    }

    if (event.type === 'turn.failed')
      connectionNotices = []
    if (event.type === 'turn.completed' || (type === 'command_execution' && event.type === 'item.completed' && item.exit_code === 0 && item.status !== 'failed'))
      recovered()
    const itemId = text(item.id)
    // A subagent keeps its row when its updates arrive after the turn that started it.
    const parentArtifact = subagents.get(agentKey(event, text(item.parent_id)))
    const agentId = type === 'subagent' ? subagents.get(agentKey(event, itemId))?.id ?? `${turn}:${agentKey(event, itemId)}` : undefined
    const id = agentId || (itemId ? `${parentArtifact?.id ?? turn}:${itemId}` : `event:${event.id}`)
    const legacy = !event.payload && !Object.keys(data).length
    const legacyMessage = legacy && event.type === 'item.completed' && !legacyTool && event.text.trim()
    const parent = parentArtifact?.subagent
    if (type === 'subagent_message' && parent) {
      parent.message = text(item.text)
      continue
    }

    // The agent's own plan shows apart from its tools. A call that failed did not change it.
    if (type === 'todo_list' && !text(item.parent_id)) {
      const steps = planSteps(item)
      if (!steps.length || text(item.status) === 'failed')
        continue
      const explanation = text(item.explanation)
      if (plan && samePlan(plan.plan.steps, steps)) {
        const previous = plan.plan
        const updatedExplanation = explanation || previous.explanation
        const activityChanged = steps.some((step, index) => step.activeText !== previous.steps[index]!.activeText)
        if (activityChanged || updatedExplanation !== previous.explanation) {
          plan.plan = {
            ...previous,
            steps,
            explanation: updatedExplanation,
            updatedAt: event.createdAt,
          }
        }

        continue
      }

      if (!plan || !continuesPlan(plan.plan.steps, steps)) {
        plan = {
          kind: 'plan',
          id: `plan:${event.runId}:${event.id}`,
          plan: {
            steps,
            explanation,
            time: event.createdAt,
            updatedAt: event.createdAt,
          },
        }
        entries.push(plan)
        continue
      }

      const change = planChange(plan.plan.steps, steps)
      plan.plan = {
        ...plan.plan,
        steps,
        explanation: explanation || plan.plan.explanation,
        updatedAt: event.createdAt,
      }
      if (change) {
        entries.push({
          kind: 'plan-update',
          id: `plan-update:${event.runId}:${event.id}`,
          planId: plan.id,
          time: event.createdAt,
          change,
          progress: planProgress(steps),
        })
      }

      continue
    }

    if (type === 'agent_message' || legacyMessage) {
      recovered()
      const content = text(item.text) || event.text
      const existing = messages.get(id)
      if (existing) {
        existing.text = content
      }
      else {
        const message = {
          kind: 'message' as const,
          id,
          time: event.createdAt,
          text: content,
          ...(via ? { via } : {}),
        }
        entries.push(message)
        messages.set(id, message)
      }

      continue
    }

    if (legacyTool && legacy && event.type === 'item.completed') {
      legacyTool.kind = 'output'
      Object.assign(legacyTool, outputSummary(event.text))
      legacyTool.status = 'info'
      legacyTool.historical = true
      legacyTool.durationMs = Math.max(0, event.createdAt - legacyTool.time)
      legacyTool.blocks = [{ label: 'Output', code: event.text, language: 'plaintext' }]
      legacyTool.raw = event.text
      legacyTool = undefined
      continue
    }

    const status = text(item.status)
    const failed = status === 'failed' || event.type.includes('failed') || event.type === 'error' || type === 'error' || (event.type === 'status' && event.text === 'failed') || (typeof item.exit_code === 'number' && item.exit_code !== 0)
    const running = !failed && (status === 'in_progress' || event.type.endsWith('.started')) && event.type.startsWith('item.')
    const artifact: ActivityArtifact = {
      id,
      sourceIds: [`${event.runId}:${event.id}`],
      time: event.createdAt,
      kind: 'notice',
      title: 'Worker update',
      subtitle: '',
      status: failed ? 'error' : running ? 'running' : 'done',
      blocks: [],
      files: [],
      tasks: [],
      raw: Object.keys(data).length ? pretty(data) : event.text,
      attention: failed || (event.type === 'status' && ['cancelled', 'interrupted'].includes(event.text)),
    }
    const block = (label: string, value: unknown, language = 'plaintext') => {
      if (value !== undefined && value !== null && value !== '')
        artifact.blocks.push({ label, code: pretty(value), language })
    }

    if (type === 'command_execution') {
      const command = describeCommand(text(item.command))
      artifact.kind = command.kind
      artifact.title = command.title
      artifact.subtitle = command.subtitle
      artifact.command = command.command
      artifact.cwd = text(item.cwd) || text(item.working_directory)
      artifact.files = command.paths.map(path => ({ path, kind: 'read' }))
      block('Command', command.command, 'bash')
      block(command.kind === 'read' ? 'File content' : command.kind === 'search' ? 'Matches' : command.kind === 'browse' ? 'Files found' : 'Output', item.aggregated_output, command.language)
      if (typeof item.exit_code === 'number')
        artifact.exitCode = item.exit_code
      const outcome = expectedCommandOutcome(text(item.command), artifact.exitCode)
      if (outcome && !item.error && event.type === 'item.completed') {
        artifact.status = 'info'
        artifact.statusLabel = outcome
      }
    }
    else if (type === 'file_change') {
      artifact.kind = 'files'
      artifact.title = 'File changes'
      for (const change of Array.isArray(item.changes) ? item.changes : []) {
        const file = record(change)
        artifact.files.push({ path: text(file.path), kind: text(file.kind) || 'update' })
        block(text(file.path) || 'Changes', file.diff ?? file.patch, 'diff')
      }

      artifact.subtitle = artifact.files.map(file => file.path).join(' · ')
    }
    else if (type === 'mcp_tool_call' || type === 'web_search') {
      artifact.kind = type === 'web_search' ? 'search' : 'tool'
      artifact.title = type === 'web_search' ? 'Searched the web' : readable(text(item.tool) || 'Tool call')
      artifact.subtitle = text(item.query) || text(record(item.action).query) || text(item.server)
      block('Arguments', item.arguments, 'json')
      const result = record(item.result)
      for (const content of Array.isArray(result.content) ? result.content : []) {
        const part = record(content)
        block('Result', part.text)
      }

      if (!artifact.blocks.some(entry => entry.label === 'Result'))
        block('Result', item.result, 'json')
      block('Error', item.error, 'json')
      if (item.error || result.isError)
        artifact.status = 'error'
    }
    else if (type === 'todo_list') {
      artifact.kind = 'plan'
      artifact.title = 'Updated the plan'
      artifact.tasks = planSteps(item).map(step => ({ text: step.text, completed: step.state === 'done' }))
      artifact.subtitle = `${artifact.tasks.filter(task => task.completed).length} of ${artifact.tasks.length} complete`
    }
    else if (type === 'reasoning') {
      artifact.kind = 'thinking'
      artifact.title = 'Thinking'
      block('Reasoning summary', item.text)
    }
    else if (type === 'subagent') {
      const previous = artifacts.get(id)?.subagent
      const state = subagentStates[status] ?? 'running'
      artifact.kind = 'subagent'
      artifact.title = text(item.name) || 'Subagent'
      artifact.subtitle = text(item.agent_type) || previous?.type || ''
      artifact.status = state === 'failed' ? 'error' : state === 'running' ? 'running' : 'done'
      if (state === 'stopped')
        artifact.statusLabel = 'Stopped'
      if (typeof item.duration_ms === 'number')
        artifact.durationMs = item.duration_ms
      artifact.subagent = {
        taskId: text(item.task_id) || previous?.taskId,
        type: artifact.subtitle,
        prompt: text(item.prompt) || previous?.prompt || '',
        result: typeof item.result === 'string' ? item.result : previous?.result || '',
        message: state === 'running' && previous?.state !== 'running' ? '' : previous?.message ?? '',
        activity: text(item.activity),
        model: text(item.model) || previous?.model || '',
        background: item.background === true,
        state,
        tokens: typeof item.tokens === 'number' ? item.tokens : undefined,
        toolUses: typeof item.tool_uses === 'number' ? item.tool_uses : undefined,
        steps: previous?.steps ?? [],
      }
      block('Prompt', item.prompt)
      block('Report', item.result)
    }
    else if (type === 'collab_tool_call') {
      artifact.kind = 'tool'
      artifact.title = collaborationTitles[text(item.tool)] ?? 'Worked with a subagent'
      artifact.subtitle = text(item.prompt).split('\n')[0] ?? ''
      block('Prompt', item.prompt)
    }
    else if (type === 'subagent_message') {
      // Its subagent is outside the loaded history.
      artifact.kind = 'tool'
      artifact.title = 'Subagent message'
      block('Message', item.text)
    }
    else {
      const labels: Record<string, string> = {
        'thread.started': 'Session connected',
        'turn.started': 'Started working',
        'turn.completed': 'Finished this turn',
        'diagnostic': 'Worker diagnostic',
        'output': 'Worker output',
        'error': 'Worker reported an error',
        'item.started': 'Tool activity',
        'item.completed': 'Work completed',
        'turn.failed': 'Turn failed',
        'turn.waiting': 'Waiting for background tasks',
      }
      artifact.title = event.type === 'status' ? (event.text.includes(' ') ? event.text : readable(event.text)) : type ? readable(type) : labels[event.type] || readable(event.type)
      const message = text(item.message) || text(data.message) || text(record(data.error).message)
      if (message)
        block('Details', message)
      else if (!['status', 'thread.started', 'turn.started', 'turn.completed'].includes(event.type))
        block('Output', event.text, Object.keys(data).length ? 'json' : 'plaintext')
      if (event.type === 'diagnostic')
        artifact.status = 'info'
      const usage = record(data.usage)
      if (typeof usage.input_tokens === 'number' && typeof usage.output_tokens === 'number')
        artifact.subtitle = `${usage.input_tokens.toLocaleString()} input · ${usage.output_tokens.toLocaleString()} output tokens`
      if (event.type === 'turn.waiting') {
        const tasks = Array.isArray(data.tasks) ? data.tasks.map(task => text(record(task).description)).filter(Boolean) : []
        artifact.status = 'info'
        if (!tasks.length)
          artifact.title = 'Background tasks finished'
        artifact.subtitle = tasks.join(' · ')
      }
    }

    if (artifact.kind === 'notice' && ['error', 'diagnostic', 'item.completed'].includes(event.type) && /websocket|reconnecting\.\.\.|reconnecting\s+\d+\//i.test(artifact.raw) && /503|reconnect|falling back|connection.*(?:closed|failed)/i.test(artifact.raw)) {
      artifact.title = 'Connection interrupted'
      artifact.attention = true
      connectionNotices.push(artifact)
    }

    // A scheduled wait shows as its own card; only a call that failed stays among the tools.
    if (type === 'mcp_tool_call' && isWaitTool(item) && artifact.status !== 'error') {
      // A provider may announce the call before its arguments: keep what the same call said.
      const reason = text(toolArguments(item.arguments).reason)
      if (itemId !== spentCall) {
        if (itemId !== waitCall || reason)
          waitReason = reason
        waitCall = itemId
      }

      continue
    }

    const previous = artifacts.get(id)
    if (previous) {
      const durationMs = artifact.kind === 'subagent' ? artifact.durationMs : undefined
      const sourceIds = [...previous.sourceIds ?? [], ...artifact.sourceIds ?? []]
      Object.assign(previous, artifact, { sourceIds, time: previous.time, durationMs: running ? undefined : durationMs ?? Math.max(0, event.createdAt - previous.time) })
      continue
    }

    artifacts.set(id, artifact)
    if (artifact.kind === 'subagent' && itemId)
      subagents.set(agentKey(event, itemId), artifact)
    if (parent) {
      parent.steps.push(artifact)
      continue
    }

    const last = entries.at(-1)
    if (last?.kind === 'group')
      last.artifacts.push(artifact)
    else
      entries.push({ kind: 'group', id: `group:${id}`, artifacts: [artifact] })
    if (legacy && event.type === 'item.started')
      legacyTool = Object.assign(artifact, { kind: 'output' as const, title: 'Recorded step', historical: true })
  }

  placeScheduled()
  const knownWait = pendingWakeup && waits.get(pendingWakeup.id)
  if (knownWait && pendingWakeup?.observation && knownWait.observation === undefined)
    knownWait.observation = pendingWakeup.observation
  if (pendingWakeup && !knownWait) {
    entries.push({
      kind: 'wait',
      id: `wait:${pendingWakeup.id}`,
      time: pendingWakeup.createdAt,
      wait: { wakeup: pendingWakeup, ...(pendingWakeup.observation ? { observation: pendingWakeup.observation } : {}) },
    })
  }

  return teamEntries(chat ? chatEntries(entries) : entries, groups)
}

/** Subagents started one after another work as a team, shown apart from the agent's own steps. */
export function teamEntries(entries: ActivityEntry[], groups = new ActivityGroups()): ActivityEntry[] {
  return groups.entries(entries)
}

/** Preserve full visible groups across pagination, without counting hidden events or global IDs. */
export class ActivityGroups {
  private fullPositions = new Map<string, { position: number, group: string }>()

  entries(entries: ActivityEntry[]): ActivityEntry[] {
    const result: ActivityEntry[] = []
    const nextPositions = new Map<string, { position: number, group: string }>()
    const identities = (artifact: ActivityArtifact) => artifact.sourceIds ?? [artifact.id]
    const previous = (artifact: ActivityArtifact) => identities(artifact).map(id => this.fullPositions.get(id)).find(Boolean)
    const appendTools = (artifacts: ActivityArtifact[]) => {
      // Folding an older start can reorder the oldest activity around a notice.
      // Anchor from the newest full group so that boundary cannot shift newer groups.
      const anchor = artifacts.findLastIndex(item => previous(item))
      const offset = anchor < 0 ? 0 : previous(artifacts[anchor]!)!.position - anchor
      let group: Extract<ActivityEntry, { kind: 'group' }> | undefined
      let previousWindow: number | undefined
      for (const [index, artifact] of artifacts.entries()) {
        const window = Math.floor((offset + index) / MAX_ACTIVITY_GROUP_SIZE)
        if (!group || window !== previousWindow) {
          const saved = previous(artifact)
          const id = saved?.position === 0 ? saved.group : `group:${identities(artifact)[0]}`
          group = { kind: 'group', id, artifacts: [] }
          result.push(group)
          previousWindow = window
        }

        group.artifacts.push(artifact)
        if (group.artifacts.length === MAX_ACTIVITY_GROUP_SIZE) {
          for (const [position, item] of group.artifacts.entries()) {
            for (const id of identities(item))
              nextPositions.set(id, { position, group: group.id })
          }
        }
      }
    }

    for (const entry of entries) {
      if (entry.kind !== 'group') {
        result.push(entry)
        continue
      }

      let tools: ActivityArtifact[] = []
      let team: Extract<ActivityEntry, { kind: 'team' }> | undefined
      for (const artifact of entry.artifacts) {
        if (artifact.kind !== 'subagent') {
          tools.push(artifact)
          team = undefined
          continue
        }

        appendTools(tools)
        tools = []
        if (!team) {
          team = { kind: 'team', id: `team:${artifact.id}`, agents: [] }
          result.push(team)
        }

        team.agents.push(artifact)
      }

      appendTools(tools)
    }

    // Bound the cache to the currently loaded timeline. Partial edge groups may still grow.
    this.fullPositions = nextPositions
    return result
  }
}

/** Hide lifecycle bookkeeping in chat without removing persisted diagnostics. */
function chatEntries(entries: ActivityEntry[]): ActivityEntry[] {
  const result: ActivityEntry[] = []
  for (const entry of entries) {
    if (entry.kind !== 'group') {
      result.push(entry)
      continue
    }

    let group: Extract<ActivityEntry, { kind: 'group' }> | undefined
    for (const artifact of entry.artifacts) {
      if (artifact.kind === 'notice') {
        if (artifact.attention) {
          result.push({ kind: 'notice', id: `notice:${artifact.id}`, artifact })
          group = undefined
        }

        continue
      }

      if (!group) {
        group = { kind: 'group', id: `group:${artifact.id}`, artifacts: [] }
        result.push(group)
      }

      group.artifacts.push(artifact)
    }
  }

  return result
}
