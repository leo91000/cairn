import type { ChatQuestion } from '../../../shared/chats.ts'
import type { Service } from './service.ts'
import { z } from 'zod'
import { questionAnswerInput, questionFields } from '../../../shared/chats.ts'
import { AppError, requireValue } from './errors.ts'

const incoming = z.object({ id: z.string().regex(/^[a-f0-9]{64}$/), blocking: z.boolean(), fields: questionFields })
export class ChatQuestions {
  constructor(private service: Service) {}
  list(chatId: string) {
    return this.service.store.keys(`chat-question:${chatId}:`).map(({ data }) => data as ChatQuestion).sort((a, b) => a.createdAt - b.createdAt)
  }

  save(question: ChatQuestion) {
    this.service.store.set(`chat-question:${question.chatId}:${question.id}`, question)
    return question
  }

  receive(runId: string, input: unknown) {
    const chat = this.service.store.list('chats').find(chat => chat.runId === runId)
    const parsed = incoming.safeParse(input)
    if (!chat || !parsed.success)
      return
    const existing = this.list(chat.id).find(question => question.id === parsed.data.id)
    if (existing)
      return this.save({ ...existing, blocking: existing.status === 'pending' && parsed.data.blocking })
    return this.service.store.transaction(() => {
      const question = this.save({
        ...parsed.data,
        runId,
        chatId: chat.id,
        status: 'pending',
        createdAt: Date.now(),
      })
      this.service.notifications.enqueue(question)
      return question
    })
  }

  release(runId: string, id?: string) {
    const chat = this.service.store.list('chats').find(chat => chat.runId === runId)
    if (!chat)
      return
    for (const question of this.list(chat.id)) {
      if ((!id || question.id === id) && question.blocking)
        this.save({ ...question, blocking: false })
    }
  }

  answer(chatId: string, id: string, input: unknown) {
    this.service.chats.get(chatId)
    const question = requireValue(this.list(chatId).find(question => question.id === id), 'Question not found')
    const values = questionAnswerInput.parse(input)
    const previous = this.service.store.chatMessages(chatId).find(message => message.id === values.id)
    if (question.messageId === values.id && previous && JSON.stringify(previous.answers) === JSON.stringify(values.answers))
      return question
    if (question.status !== 'pending')
      throw new AppError(409, 'This question has already been answered.')
    if (Object.keys(values.answers).length !== question.fields.length || question.fields.some(field => !Object.hasOwn(values.answers, field.id)))
      throw new AppError(400, 'Answer each question before sending.')
    const text = `My answers to your questions:\n\n${question.fields.map(field => `${field.title}\n${values.answers[field.id]!.join('\n')}`).join('\n\n')}`
    this.service.chats.send(chatId, { id: values.id, text, mode: 'steer' }, { question, answers: values.answers })
    return this.list(chatId).find(question => question.id === id)!
  }

  acknowledge(chatId: string, id: string) {
    const question = this.list(chatId).find(question => question.id === id)
    if (question)
      this.save({ ...question, blocking: false, status: 'answered' })
  }
}
