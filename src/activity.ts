import type { RunEvent } from '../shared/contracts'
import { describeCommand, expectedCommandOutcome } from './activity-command'
import { outputSummary } from './activity-data'

export interface ActivityCode {
  label: string
  code: string
  language: string
}
export interface ActivityArtifact {
  kind: 'command' | 'read' | 'browse' | 'output' | 'files' | 'search' | 'tool' | 'plan' | 'thinking' | 'notice'
  id: string
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
}
export type ActivityEntry
  = | {
    kind: 'message'
    role?: 'user'
    id: string
    time: number
    text: string
    delivery?: string
    attachments?: import('../shared/chats').ChatAttachment[]
  }
  | { kind: 'group', id: string, artifacts: ActivityArtifact[] }
  | { kind: 'notice', id: string, artifact: ActivityArtifact }

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
    return data.type === event.type || Object.hasOwn(data, 'item') ? data : {}
  }
  catch {
    return {}
  }
}

function readable(value: string) {
  return value.replaceAll(/[._]/g, ' ').replace(/^./, letter => letter.toUpperCase())
}

/** Keep tool updates in their original position and fold actions between messages. */
export function activityEntries(events: RunEvent[], chat = false): ActivityEntry[] {
  const entries: ActivityEntry[] = []
  const artifacts = new Map<string, ActivityArtifact>()
  const messages = new Map<string, Extract<ActivityEntry, { kind: 'message' }>>()
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
  let legacyTool: ActivityArtifact | undefined
  for (const event of events) {
    if (event.type === 'artifact')
      continue
    if (event.type === 'chat.user') {
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
    const item = record(data.item)
    const type = text(item.type)
    if (event.type === 'turn.started') {
      connectionNotices = []
      turn++
    }

    if (event.type === 'turn.failed')
      connectionNotices = []
    if (event.type === 'turn.completed' || (type === 'command_execution' && event.type === 'item.completed' && item.exit_code === 0 && item.status !== 'failed'))
      recovered()
    const itemId = text(item.id)
    const id = itemId ? `${turn}:${itemId}` : `event:${event.id}`
    const legacy = !event.payload && !Object.keys(data).length
    const legacyMessage = legacy && event.type === 'item.completed' && !legacyTool && event.text.trim()
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
      artifact.tasks = (Array.isArray(item.items) ? item.items : []).map(value => ({ text: text(record(value).text), completed: record(value).completed === true }))
      artifact.subtitle = `${artifact.tasks.filter(task => task.completed).length} of ${artifact.tasks.length} complete`
    }
    else if (type === 'reasoning') {
      artifact.kind = 'thinking'
      artifact.title = 'Thinking'
      block('Reasoning summary', item.text)
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
      }
      artifact.title = event.type === 'status' ? readable(event.text) : type ? readable(type) : labels[event.type] || readable(event.type)
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
    }

    if (artifact.kind === 'notice' && ['error', 'diagnostic', 'item.completed'].includes(event.type) && /websocket|reconnecting\.\.\.|reconnecting\s+\d+\//i.test(artifact.raw) && /503|reconnect|falling back|connection.*(?:closed|failed)/i.test(artifact.raw)) {
      artifact.title = 'Connection interrupted'
      artifact.attention = true
      connectionNotices.push(artifact)
    }

    const previous = artifacts.get(id)
    if (previous) {
      Object.assign(previous, artifact, { time: previous.time, durationMs: running ? undefined : Math.max(0, event.createdAt - previous.time) })
      continue
    }

    artifacts.set(id, artifact)
    const last = entries.at(-1)
    if (last?.kind === 'group')
      last.artifacts.push(artifact)
    else
      entries.push({ kind: 'group', id: `group:${id}`, artifacts: [artifact] })
    if (legacy && event.type === 'item.started')
      legacyTool = Object.assign(artifact, { kind: 'output' as const, title: 'Recorded step', historical: true })
  }

  return chat ? chatEntries(entries) : entries
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
