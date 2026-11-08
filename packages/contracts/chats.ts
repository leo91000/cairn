import type { ChatQuestion } from './generated/typescript/ChatQuestion'
import type { QuestionAnswer } from './generated/typescript/QuestionAnswer'
import type { WaitCondition } from './generated/typescript/WaitCondition'

export type { Chat, Chat as ChatDetail, Chat as ChatView } from './generated/typescript/Chat'
export type { ChatAttachment } from './generated/typescript/ChatAttachment'
export type { ChatCompaction } from './generated/typescript/ChatCompaction'
export type { ChatExecution } from './generated/typescript/ChatExecution'
export type { ChatMessage } from './generated/typescript/ChatMessage'
export type { ChatQuestion } from './generated/typescript/ChatQuestion'
export type { ChatSideQuestion } from './generated/typescript/ChatSideQuestion'

export type { ChatWakeup } from './generated/typescript/ChatWakeup'
export type { ContextUsage } from './generated/typescript/ContextUsage'
export type { ConversationLifecycle } from './generated/typescript/ConversationLifecycle'
export type { QuestionAnswer } from './generated/typescript/QuestionAnswer'
export type { QuestionField } from './generated/typescript/QuestionField'
export type { QuestionOption } from './generated/typescript/QuestionOption'
export type { WaitCondition } from './generated/typescript/WaitCondition'
export type { WaitDisplay } from './generated/typescript/WaitDisplay'
export type { WaitMemberProgress } from './generated/typescript/WaitMemberProgress'
export type { WaitObservation } from './generated/typescript/WaitObservation'

export type WaitGroup = Extract<WaitCondition, { type: 'group' }>
export type PluginCondition = Extract<WaitCondition, { type: 'plugin' }>

export function validQuestionAnswer(field: ChatQuestion['fields'][number], answer: QuestionAnswer): boolean {
  if (!answer.selected.length)
    return !!answer.comment.trim()

  const maximum = field.mode === 'single' ? 1 : (field.maxChoices ?? field.options.length)
  return answer.selected.length >= (field.minChoices ?? 1)
    && answer.selected.length <= maximum
    && new Set(answer.selected).size === answer.selected.length
    && answer.selected.every(id => field.options.some(option => option.id === id))
}

export type { WaitResultDisplay } from './generated/typescript/WaitResultDisplay'
