import type { Run } from './contracts'
import { z } from 'zod'
import { MAIN_AGENT_ID } from './constants'
import { reasoningEffort } from './contracts'

export const chatInput = z.object({
  agentId: z.string().uuid().default(MAIN_AGENT_ID),
  projectId: z.string().uuid().nullable().default(null),
})
export const chatMessageInput = z.object({
  id: z.string().uuid(),
  text: z.string().trim().max(50000).default(''),
  attachmentIds: z.array(z.string().uuid()).max(8).default([]),
  mode: z.enum(['queue', 'steer']).default('queue'),
  provider: z.enum(['', 'codex', 'claude']).default(''),
  // Rust's JSON Schema regex parser requires the opening bracket to be escaped.
  // eslint-disable-next-line regexp/no-useless-escape
  model: z.string().trim().max(120).regex(/^[\w./:\[\]-]*$/).default(''),
  reasoning: reasoningEffort.default(''),
}).refine(message => message.text.length > 0 || message.attachmentIds.length > 0, 'Write a message or attach a file')
export interface ChatAttachment {
  id: string
  chatId: string
  name: string
  size: number
  mediaType: string
  kind: 'image' | 'file'
}
export type ConversationLifecycle = 'active' | 'trash' | 'purging'
export interface Chat {
  lifecycle?: ConversationLifecycle
  trashedAt?: number | null
  purgeAt?: number | null
  restoredAt?: number
  sessionRestartRequested?: boolean
  lifecycleError?: string | null
  pendingMessages?: number
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
  provider?: '' | 'codex' | 'claude'
  model: string
  reasoning?: string
  mode: 'queue' | 'steer'
  status: 'queued' | 'sending' | 'delivered' | 'cancelled'
  createdAt: number
}
export interface ChatView extends Chat {
  pendingQuestions: number
  agentName: string
  projectName: string | null
  status: Run['status'] | 'idle'
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
}
