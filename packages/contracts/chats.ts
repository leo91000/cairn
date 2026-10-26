import type { Provider } from './accounts'
import type { Run } from './contracts'
import { z } from 'zod'
import { MAIN_AGENT_ID } from './constants'
import { reasoningEffort, serviceSpeed } from './contracts'

export const chatInput = z.object({
  agentId: z.string().uuid().default(MAIN_AGENT_ID),
  projectId: z.string().uuid().nullable().default(null),
})
export const chatMessageInput = z.object({
  id: z.string().uuid(),
  text: z.string().trim().max(50000).default(''),
  attachmentIds: z.array(z.string().uuid()).max(8).default([]),
  mode: z.enum(['queue', 'steer']).default('queue'),
  provider: z.enum(['', 'codex', 'claude', 'opencode']).default(''),
  // Rust's JSON Schema regex parser requires the opening bracket to be escaped.
  // eslint-disable-next-line regexp/no-useless-escape
  model: z.string().trim().max(120).regex(/^[\w./:\[\]-]*$/).default(''),
  reasoning: reasoningEffort.default(''),
  plan: z.boolean().default(false).describe('Plan turn: the agent explores and proposes an approach without changing files, using shell network access or calling tools with external effects. Send a later message without plan to implement it. Unavailable with OpenCode.'),
  speed: serviceSpeed.default(''),
}).refine(message => message.text.length > 0 || message.attachmentIds.length > 0, 'Write a message or attach a file')
export interface ChatAttachment {
  id: string
  chatId: string
  name: string
  size: number
  mediaType: string
  kind: 'image' | 'file'
  /** An image's pixel size as it displays; absent for files and older uploads. */
  width?: number
  height?: number
}
export type ConversationLifecycle = 'active' | 'trash' | 'purging'
/** A condition a plugin provides, such as `github_pr_checks`, with its own fields. */
export interface PluginCondition {
  type: string
  pluginId: string
  projectId?: string
  [field: string]: unknown
}
/** A plugin's words for a pending wait. */
export interface WaitDisplay {
  title: string
  short: string
  details: string[]
}
/** A plugin's words for a wait's result, inside its observation. */
export interface WaitResultDisplay {
  tone: 'ok' | 'error' | 'warning' | 'neutral'
  title: string
  detail: string
}
/** One external condition: a run, a delegated task, or a plugin's condition. */
export type WaitCondition = { type: 'run_finished', runId: string } | { type: 'task_finished', chatId: string } | PluginCondition
/** Several external conditions in one wait: `all` resumes once each has a result, `any` at the first. */
export interface WaitGroup {
  type: 'group'
  mode: 'all' | 'any'
  conditions: WaitCondition[]
}
/** What Cairn knows of one grouped condition while the wait lasts. */
export interface WaitMemberProgress {
  /** Its result, once it has one; failures and cancellations included. */
  observation?: Record<string, unknown> | null
  /** What its plugin shows while it waits. */
  display?: WaitDisplay | null
  nextCheckAt?: number
  lastError?: string
}
export interface ChatWakeup {
  condition?: WaitCondition | WaitGroup
  /** A group's progress, in the order of its conditions. */
  members?: WaitMemberProgress[]
  observation?: Record<string, unknown> | null
  /** What a plugin condition shows while waiting. */
  display?: WaitDisplay | null
  /** Plugin conditions: when Cairn checks again, and why its last check failed. */
  nextCheckAt?: number
  lastError?: string
  id: string
  messageId: string | null
  reason: string
  seconds: number
  createdAt: number
  wakeAt: number
}
export interface Chat {
  wakeup?: ChatWakeup | null
  slug?: string
  lifecycle?: ConversationLifecycle
  trashedAt?: number | null
  purgeAt?: number | null
  restoredAt?: number
  sessionRestartRequested?: boolean
  canRequestNewSession?: boolean
  lifecycleError?: string | null
  pendingMessages?: number
  /** The conversation whose agent delegated this one, and its retry key. */
  requestedBy?: { runId: string, chatId: string, requestId?: string | null } | null
  id: string
  title: string
  agentId: string
  projectId: string | null
  runId: string | null
  paused: boolean
  createdAt: number
  updatedAt: number
}
export interface ChatMessage {
  attachments?: ChatAttachment[]
  questionId?: string
  answers?: Record<string, string[]>
  id: string
  chatId: string
  text: string
  provider?: '' | 'codex' | 'claude' | 'opencode'
  model: string
  reasoning?: string
  /** Absent on messages saved before plan turns, which were never plan turns. */
  plan?: boolean
  /** The Codex speed this message asked for; absent on earlier messages. */
  speed?: string
  mode: 'queue' | 'steer'
  status: 'queued' | 'sending' | 'delivered' | 'cancelled'
  createdAt: number
}
/**
 * How full the coding agent's context was at its latest report. `contextTokens` is what the
 * latest model request occupied, never the conversation's cumulative spend.
 */
export interface ContextUsage {
  contextTokens: number
  /** The model's context window; absent when the coding agent does not report it. */
  contextWindow: number | null
  /** Of `contextTokens`, input read from the prompt cache, when reported. */
  cachedTokens: number | null
  /** A local estimate: before the session's first response, or right after a compaction. */
  estimated: boolean
}
/** Whether the coding agent can compact its context now, and why not when it cannot. */
export interface ChatCompaction {
  /** The coding agent offers a compaction control. OpenCode does not. */
  supported: boolean
  running: boolean
  available: boolean
  reason: string | null
}
export interface ChatView extends Chat {
  suspendedByExclusive?: Run['suspendedByExclusive']
  pendingQuestions: number
  agentName: string
  projectName: string | null
  status: Run['status'] | 'idle'
  /** The coding agent of the latest turn; a message may switch it from the agent's own. */
  provider?: Provider | null
  /** When the agent's latest turn ended. */
  finishedAt?: number | null
  /** Absent for older servers and before the coding agent reports it. */
  contextUsage?: ContextUsage | null
  compaction?: ChatCompaction
  /** The agent finished a turn the signed-in account has not opened since. */
  unread?: boolean
  /** The opening of the agent's final message, while unread. */
  preview?: string | null
  /** The conversation that delegated this one, while it still exists. */
  requester?: { chatId: string, title: string } | null
}
export interface ChatDetail extends ChatView {
  questions: ChatQuestion[]
  run: Run | null
  messages: ChatMessage[]
}
export const questionFields = z.array(z.object({
  id: z.string().min(1).max(200),
  title: z.string().min(1).max(10000),
  secret: z.boolean().default(false),
  options: z.array(z.object({ label: z.string().max(2000), description: z.string().max(4000).default('') })).max(30).default([]),
})).min(1).max(20).refine(fields => new Set(fields.map(field => field.id)).size === fields.length, 'Question identifiers must be unique')
export interface ChatQuestion {
  id: string
  chatId: string
  runId: string
  blocking: boolean
  fields: z.infer<typeof questionFields>
  status: 'pending' | 'answering' | 'answered' | 'cancelled'
  createdAt: number
  messageId?: string
}
export const questionAnswerInput = z.object({
  id: z.string().uuid(),
  answers: z.record(z.string(), z.array(z.string().trim().min(1).max(10000)).min(1).max(1)),
})
export interface ChatExecution {
  attachments?: ChatAttachment[]
  messageId: string
  text: string
  recovery: boolean
  /** A plan turn, which explores and proposes without changing anything. */
  plan?: boolean
}
