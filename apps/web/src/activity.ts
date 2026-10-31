import type { ChatWakeup, WaitObservation } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import type { AgentPlan, PlanChange, PlanProgress } from './agent-plan'
import type { CompactionEntry } from './context-usage'
import { describeCommand, expectedCommandOutcome } from './activity-command'
import {
  continuesPlan,
  planChange,
  planProgress,
  planSteps,
  samePlan,
} from './agent-plan'
import { compactionEnd } from './context-usage'
import { noticeArtifact } from './notices'
import { questionAnswerText } from './question-answer'

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
  /** The agent resumed it after it reported back; it keeps its identity and steps. */
  resumed?: boolean
  state: 'running' | 'done' | 'failed' | 'stopped'
  tokens?: number
  toolUses?: number
  steps: ActivityArtifact[]
  /** A task Cairn delegated to another conversation, followed through that conversation. */
  delegation?: { chatId: string, agentName: string, provider: string }
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
  statusLabel?: string
  attention?: boolean
  /** Process output must be displayed as written, without interpreting embedded data. */
  verbatim?: boolean
  subagent?: ActivitySubagent
}
export type ActivityEntry
  = | {
    kind: 'message'
    role?: 'user'
    id: string
    time: number
    text: string
    /** The person's message this card delivered. */
    messageId?: string
    delivery?: string
    /** The model provider that wrote this reply, when the engine chose among several. */
    via?: string
    attachments?: import('../../../packages/contracts/chats').ChatAttachment[]
    /** A user request for a plan turn, which explores without changing anything. */
    plan?: boolean
  }
  /** The approach a plan turn proposed; the user decides whether to implement it. */
  | {
    kind: 'proposal'
    id: string
    time: number
    text: string
    /** Only a completed proposal can be approved. */
    sourceId?: string
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
  /** The coding agent compacted its context; the history above stays as it was. */
  | CompactionEntry

/** A durable wait the agent scheduled with `sleep` or `wait_for`, then how it ended. */
export interface ActivityWait {
  /** Absent when the condition had already completed and the agent never waited. */
  wakeup: ChatWakeup | null
  /** What Cairn observed once the condition completed or the deadline passed. */
  observation?: WaitObservation | Record<string, unknown> | null
  /** When the conversation resumed. */
  resumedAt?: number
  /** A message from the user or a failed turn cancelled it, or another wait replaced it. */
  ended?: 'cancelled' | 'replaced'
  /** The agent's reason, for a result that was already available: no wakeup carries it. */
  reason?: string
}

// Where a scheduled wait's card is placed: its turn ended, or a message or another wait event came.
const waitBoundaries = new Set(['turn.started', 'turn.completed', 'turn.failed', 'chat.user', 'chat.sleep', 'chat.wait', 'chat.wait_ready', 'chat.wakeup', 'run.wait', 'run.wait_ready', 'run.wakeup'])
const terminalStatuses = new Set(['succeeded', 'failed', 'cancelled', 'interrupted'])

// Codex names the workspace server; Claude and OpenCode prefix the tool with it.
const workspaceWaitTool = /^(?:mcp__)?cairn_workspace(?:__|_|\.)(?:sleep|wait_for)$/

const workspaceDelegateTool = /^(?:mcp__)?cairn_workspace(?:__|_|\.)delegate_task$/

/** Its result shows as the delegated task Cairn records, rather than as a tool call. */
function isDelegateTool(item: Record<string, unknown>) {
  const tool = text(item.tool)
  return workspaceDelegateTool.test(tool) || (text(item.server) === 'cairn_workspace' && tool === 'delegate_task')
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

function activityPayload(event: RunEvent) {
  return event.payload ?? {}
}

/** Whether a reason repeats one already given; a run's recorded error may be truncated. */
export function sameReason(given: string, reason: string) {
  return given === reason || (!!reason && given.startsWith(reason))
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

/** Approval identity is distinct from the row anchored for reading. */
export function proposedPlanApproval(events: RunEvent[], cards: TimelineEntry[] = []): string | undefined {
  let latest: RunEvent | undefined
  let lastRequest: RunEvent | undefined
  for (const event of events) {
    if (event.type === 'chat.user' && (!lastRequest || event.id > lastRequest.id))
      lastRequest = event
    if (!['item.started', 'item.updated', 'item.completed'].includes(event.type))
      continue
    const item = record(record(event.payload).item)
    if (item.type === 'proposed_plan' && !text(item.parent_id) && (!latest || event.id > latest.id))
      latest = event
  }

  const requestCard = cards.filter(card => card.content.kind === 'userMessage').sort((a, b) => b.source - a.source)[0]
  const requestSource = Math.max(lastRequest?.id ?? -1, requestCard?.source ?? -1)
  const requestId = requestCard?.source === requestSource && requestCard.content.kind === 'userMessage'
    ? requestCard.content.messageId
    : record(lastRequest?.payload).messageId
  if (!latest || latest.type !== 'item.completed' || latest.id <= requestSource)
    return undefined
  const item = record(record(latest.payload).item)
  if (!text(item.text).trim())
    return undefined
  if ('request_id' in item && (!text(item.request_id) || (requestSource >= 0 && text(item.request_id) !== requestId)))
    return undefined
  return `${latest.runId}:${latest.id}`
}

/**
 * Keep tool updates in their original position and fold actions between messages. Timeline
 * cards (messages, the run cycle, errors and retries) arrive interpreted by the server; they
 * take their place among the activity, and the events they present only mark boundaries.
 */
export function activityEntries(events: RunEvent[], chat = false, groups = new ActivityGroups(), pendingWakeup?: ChatWakeup | null, cards: TimelineEntry[] = []): ActivityEntry[] {
  const entries: ActivityEntry[] = []
  const artifacts = new Map<string, ActivityArtifact>()
  const proposals = new Map<string, Extract<ActivityEntry, { kind: 'proposal' }>>()
  // By the agent's own item ID: a subagent's steps can arrive after the turn that started it.
  const subagents = new Map<string, ActivityArtifact>()
  const aliases = new Map<string, string>()
  for (const event of events) {
    const item = record(activityPayload(event).item)
    if (item.type === 'subagent' && text(item.replaces_id))
      aliases.set(`${event.runId}:${text(item.replaces_id)}`, text(item.id))
  }

  const agentKey = (event: RunEvent, id: string) => `${event.runId}:${aliases.get(`${event.runId}:${id}`) ?? id}`
  let turn = 0
  // The plan its updates change, until the agent finishes it or writes another one.
  let plan: Extract<ActivityEntry, { kind: 'plan' }> | undefined
  // The wait still pending, by its wakeup ID, so that its result and resumption complete it.
  const waits = new Map<string, ActivityWait>()
  let openWait: ActivityWait | undefined
  // A scheduled wait only starts once its turn ends: its card follows what the agent did after
  // the call, and is placed when the turn ends, another wait or a message comes, or the
  // history does.
  let scheduled: Extract<ActivityEntry, { kind: 'wait' }> | undefined
  const placeScheduled = () => {
    if (scheduled)
      entries.push(scheduled)
    scheduled = undefined
  }

  const sources = new Map(events.map(event => [event.id, event]))
  let nextCard = 0
  const placeCards = (before: number) => {
    for (; nextCard < cards.length && cards[nextCard]!.position < before; nextCard++) {
      const card = cards[nextCard]!
      const content = card.content
      if (content.kind === 'userMessage') {
        entries.push({
          kind: 'message',
          role: 'user',
          id: card.id,
          time: content.sentAt,
          text: questionAnswerText(content.text, content.questionId),
          attachments: content.attachments,
          ...(content.plan === true ? { plan: true } : {}),
          ...(content.messageId ? { messageId: content.messageId } : {}),
        })
        continue
      }

      if (content.kind === 'agentMessage') {
        // A reply repeating a failure the conversation reports stays in the run log.
        if (!chat || !content.echo) {
          entries.push({
            kind: 'message',
            id: card.id,
            time: card.createdAt,
            text: content.text,
            ...(content.via ? { via: content.via } : {}),
          })
        }

        continue
      }

      const artifact = noticeArtifact(card, content.notice, sources.get(card.source))
      const last = entries.at(-1)
      if (last?.kind === 'group')
        last.artifacts.push(artifact)
      else
        entries.push({ kind: 'group', id: `group:${artifact.id}`, artifacts: [artifact] })
    }
  }

  // By run and item: a compaction's start and end update one divider.
  const compactions = new Map<string, CompactionEntry>()
  for (const [eventIndex, stored] of events.entries()) {
    const event = stored
    placeCards(event.id)
    // A compaction still running when its turn or run ends did not complete.
    const end = compactionEnd(event)
    if (end) {
      for (const [key, compaction] of compactions) {
        if (compaction.state === 'running' && key.startsWith(`${event.runId}:`))
          compaction.state = end
      }
    }

    // Context usage is a measurement the conversation header shows, not an activity.
    if (event.type === 'artifact' || event.type === 'context.usage')
      continue
    // A delegated task works like a subagent, in a conversation of its own.
    if (event.type === 'chat.delegated') {
      const data = record(event.payload)
      const chatId = text(data.chatId)
      const id = `delegated:${chatId}`
      if (!chatId || artifacts.has(id))
        continue
      const agentName = text(data.agentName)
      const artifact: ActivityArtifact = {
        id,
        sourceIds: [`${event.runId}:${event.id}`],
        time: event.createdAt,
        kind: 'subagent',
        title: text(data.title) || event.text || 'Delegated task',
        subtitle: agentName,
        status: 'running',
        blocks: [],
        files: [],
        tasks: [],
        raw: pretty(data),
        subagent: {
          type: agentName,
          prompt: text(data.task),
          result: '',
          message: '',
          activity: '',
          model: text(data.model),
          background: true,
          state: 'running',
          steps: [],
          delegation: { chatId, agentName, provider: text(data.provider) },
        },
      }
      artifacts.set(id, artifact)
      const last = entries.at(-1)
      if (last?.kind === 'group')
        last.artifacts.push(artifact)
      else
        entries.push({ kind: 'group', id: `group:${id}`, artifacts: [artifact] })
      continue
    }

    if (waitBoundaries.has(event.type) || (event.type === 'status' && terminalStatuses.has(event.text)))
      placeScheduled()
    if (['chat.sleep', 'chat.wait', 'run.wait'].includes(event.type)) {
      const wakeup = record(event.payload) as unknown as ChatWakeup
      if (!wakeup.id || waits.has(wakeup.id))
        continue
      if (openWait)
        openWait.ended = 'replaced'
      openWait = { wakeup }
      waits.set(wakeup.id, openWait)
      scheduled = {
        kind: 'wait',
        id: `wait:${wakeup.id}`,
        time: event.createdAt,
        wait: openWait,
      }
      continue
    }

    // The condition completed right away: the agent never waited, and this call replaced any wait.
    const observation = ['chat.wait_ready', 'run.wait_ready'].includes(event.type) ? activityPayload(event) : {}
    const immediate = observation.immediate === true
    if (['chat.wait_ready', 'run.wait_ready'].includes(event.type) && immediate) {
      if (openWait) {
        openWait.ended = 'replaced'
        openWait = undefined
      }

      const reason = text(observation.reason)
      entries.push({
        kind: 'wait',
        id: `wait-ready:${event.id}`,
        time: event.createdAt,
        wait: { wakeup: null, observation, ...(reason ? { reason } : {}) },
      })
      continue
    }

    // Polling found the result while the conversation waited. When the scheduling is outside the
    // loaded page, a later resumption or the live snapshot identifies the wait.
    if (['chat.wait_ready', 'run.wait_ready'].includes(event.type)) {
      const following = events.slice(eventIndex + 1)
      const boundary = following.findIndex(next => ['chat.user', 'chat.wait', 'chat.sleep', 'run.wait'].includes(next.type))
      const continuation = boundary < 0 ? following : following.slice(0, boundary)
      const resumed = continuation.find(next => ['chat.wakeup', 'run.wakeup'].includes(next.type)
        && JSON.stringify(next.payload?.observation) === JSON.stringify(observation))
      const open = openWait?.wakeup?.condition ? openWait.wakeup : undefined
      const resumedWakeup = resumed?.payload as unknown as ChatWakeup | undefined
      const pending = pendingWakeup?.condition && pendingWakeup.createdAt <= event.createdAt ? pendingWakeup : undefined
      const target = open ?? resumedWakeup ?? pending
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

    if (['chat.wakeup', 'run.wakeup'].includes(event.type)) {
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

      placeCards(event.id + 1)
      continue
    }

    const data = activityPayload(event)
    const item = record(data.item)
    const type = text(item.type)
    if (type === 'context_compaction' && event.type.startsWith('item.')) {
      const key = `${event.runId}:${text(item.id) || event.id}`
      const status = text(item.status)
      const count = (value: unknown) => typeof value === 'number' && value > 0 ? value : null
      const trigger = text(item.trigger)
      const compaction = compactions.get(key) ?? {
        kind: 'compaction' as const,
        id: `compaction:${key}`,
        time: event.createdAt,
        state: 'running' as const,
        trigger: null,
        preTokens: null,
        postTokens: null,
      }
      compaction.state = status === 'failed' ? 'failed' : status === 'completed' || event.type === 'item.completed' ? 'done' : 'running'
      compaction.trigger = trigger === 'auto' || trigger === 'manual' ? trigger : compaction.trigger
      compaction.preTokens = count(item.pre_tokens) ?? compaction.preTokens
      compaction.postTokens = count(item.post_tokens) ?? compaction.postTokens
      if (!compactions.has(key)) {
        compactions.set(key, compaction)
        entries.push(compaction)
      }

      continue
    }

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

    // Item IDs may repeat in a later turn: a new turn's calls start afresh.
    if (event.type === 'turn.started')
      turn++
    // The timeline presents this event: its card takes its place here.
    if (event.timeline) {
      placeCards(event.id + 1)
      continue
    }

    const itemId = text(item.id)
    // A subagent keeps its row when its updates arrive after the turn that started it.
    const parentArtifact = subagents.get(agentKey(event, text(item.parent_id)))
    const agentId = type === 'subagent' ? subagents.get(agentKey(event, itemId))?.id ?? `${turn}:${agentKey(event, itemId)}` : undefined
    const id = agentId || (itemId ? `${parentArtifact?.id ?? turn}:${itemId}` : `event:${event.id}`)
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

    if (type === 'proposed_plan' && !text(item.parent_id)) {
      const content = text(item.text)
      const sourceId = event.type === 'item.completed' ? `${event.runId}:${event.id}` : undefined
      const existing = proposals.get(id)
      if (existing) {
        existing.text = content || existing.text
        existing.sourceId = sourceId
      }
      else if (content) {
        const proposal = {
          kind: 'proposal' as const,
          id,
          time: event.createdAt,
          text: content,
          sourceId,
        }
        entries.push(proposal)
        proposals.set(id, proposal)
      }

      continue
    }

    const status = text(item.status)
    const failed = status === 'failed' || event.type.includes('failed') || (typeof item.exit_code === 'number' && item.exit_code !== 0)
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
      attention: failed,
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
        resumed: item.resumed === true || previous?.resumed === true,
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
    else if (type === 'proposed_plan') {
      // A child proposal is its activity, never the conversation's approval target.
      artifact.kind = 'tool'
      artifact.title = 'Subagent proposed plan'
      block('Plan', item.text, 'markdown')
    }
    else if (type === 'subagent_message') {
      // Its subagent is outside the loaded history.
      artifact.kind = 'tool'
      artifact.title = 'Subagent message'
      block('Message', item.text)
    }
    else {
      const labels: Record<string, string> = {
        'output': 'Worker output',
        'item.started': 'Tool activity',
        'item.completed': 'Work completed',
      }
      artifact.title = type ? readable(type) : labels[event.type] || readable(event.type)
      const message = text(item.message) || text(data.message) || text(record(data.error).message)
      if (message)
        block('Details', message)
      else
        block('Output', event.text, Object.keys(data).length ? 'json' : 'plaintext')
    }

    if (type === 'mcp_tool_call' && isDelegateTool(item) && artifact.status !== 'error')
      continue

    // A scheduled wait shows as its own card; only a call that failed stays among the tools.
    if (type === 'mcp_tool_call' && isWaitTool(item) && artifact.status !== 'error') {
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
  }

  placeCards(Infinity)
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
