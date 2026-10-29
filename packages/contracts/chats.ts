import { z } from 'zod'

export type { Chat, Chat as ChatDetail, Chat as ChatView } from './generated/typescript/Chat'
export type { ChatAttachment } from './generated/typescript/ChatAttachment'
export type { ChatCompaction } from './generated/typescript/ChatCompaction'
export type { ChatExecution } from './generated/typescript/ChatExecution'
export type { ChatMessage } from './generated/typescript/ChatMessage'
export type { ContextUsage } from './generated/typescript/ContextUsage'

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
export const questionFields = z.array(z.object({
  id: z.string().min(1).max(200),
  title: z.string().min(1).max(10000),
  secret: z.boolean().default(false),
  mode: z.enum(['single', 'multiple']).default('single'),
  minChoices: z.number().int().min(1).max(30).optional(),
  maxChoices: z.number().int().min(1).max(30).optional(),
  options: z.array(z.object({
    id: z.string().min(1).max(200),
    label: z.string().min(1).max(2000),
    description: z.string().max(4000).default(''),
    recommended: z.boolean().default(false),
  })).max(30).default([]),
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
  answers: z.record(z.string(), z.object({
    selected: z.array(z.string().min(1).max(200)).max(30).default([]),
    comment: z.string().trim().max(10000).default(''),
  })),
})
export const askQuestionsInput = z.object({
  requestId: z.string().uuid(),
  questions: questionFields,
}).describe('Create a questionnaire of at most 64 KiB of encoded JSON. Replies arrive asynchronously.')
export type QuestionAnswer = z.infer<typeof questionAnswerInput>['answers'][string]

export function validQuestionAnswer(field: ChatQuestion['fields'][number], answer: QuestionAnswer): boolean {
  if (!answer.selected.length)
    return !!answer.comment.trim()

  const maximum = field.mode === 'single' ? 1 : (field.maxChoices ?? field.options.length)
  return answer.selected.length >= (field.minChoices ?? 1)
    && answer.selected.length <= maximum
    && new Set(answer.selected).size === answer.selected.length
    && answer.selected.every(id => field.options.some(option => option.id === id))
}

export type { ConversationLifecycle } from './generated/typescript/ConversationLifecycle'
