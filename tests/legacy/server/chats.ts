import type {
  Chat,
  ChatDetail,
  ChatQuestion,
  ChatView,
} from '../../../shared/chats.ts'
import type { Service } from './service.ts'
import type { Worker } from './worker.ts'
import { randomUUID } from 'node:crypto'
import { mkdir, rename, writeFile } from 'node:fs/promises'
import path from 'node:path'
import { chatInput, chatMessageInput } from '../../../shared/chats.ts'
import { taskInput } from '../../../shared/contracts.ts'
import { AppError, requireValue } from './errors.ts'
import { taskProjects } from './policy.ts'

export class Chats {
  constructor(private service: Service) {}
  get store() { return this.service.store }
  get(id: string) { return requireValue(this.store.get('chats', id), 'Chat not found') }
  view(chat: Chat): ChatView {
    return {
      ...chat,
      pendingQuestions: this.service.questions.list(chat.id).filter(question => question.status === 'pending').length,
      agentName: this.store.get('agents', chat.agentId)?.name ?? 'Deleted agent',
      projectName: chat.projectId ? this.store.get('projects', chat.projectId)?.name ?? 'Deleted project' : null,
      status: chat.runId ? this.store.run(chat.runId)?.status ?? 'idle' : 'idle',
    }
  }

  detail(id: string): ChatDetail {
    const chat = this.get(id)
    return {
      ...this.view(chat),
      questions: this.service.questions.list(id),
      run: chat.runId ? this.store.run(chat.runId)! : null,
      messages: this.store.chatMessages(id),
    }
  }

  create(input: unknown) {
    const values = chatInput.parse(input)
    const agent = requireValue(this.store.get('agents', values.agentId), 'Agent not found')
    taskProjects(agent, { projectId: values.projectId }, this.store.list('projects'))
    return this.store.put('chats', {
      ...values,
      id: randomUUID(),
      title: 'New chat',
      runId: null,
      paused: false,
      createdAt: Date.now(),
      updatedAt: Date.now(),
    })
  }

  send(id: string, input: unknown, answer?: { question: ChatQuestion, answers: Record<string, string[]> }) {
    const chat = this.get(id)
    const values = chatMessageInput.parse(input)
    const messages = this.store.chatMessages(id)
    const existing = messages.find(message => message.id === values.id)
    if (existing) {
      if (existing.text !== values.text || existing.model !== values.model || (answer && existing.questionId !== answer.question.id))
        throw new AppError(409, 'This message identifier has already been used.')
      return existing
    }

    if (messages.filter(message => message.status !== 'delivered').length >= 20)
      throw new AppError(409, 'The queue is full. Wait for a reply or remove a queued message.')
    if (this.store.db.prepare('SELECT id FROM chat_messages WHERE id=?').get(values.id))
      throw new AppError(409, 'This message identifier has already been used.')
    const agent = requireValue(this.store.get('agents', chat.agentId), 'This agent is no longer available.')
    taskProjects(agent, { projectId: chat.projectId }, this.store.list('projects'))
    const run = chat.runId ? this.store.run(chat.runId) : null
    if (run?.workspaceCleanedAt)
      throw new AppError(409, 'This workspace has been cleaned up. Start a new chat.')
    // Model changes apply to the next turn; steering never silently changes a running model.
    this.validateSteer(chat, values)
    return this.store.transaction(() => {
      this.store.put('chats', { ...chat, title: messages.length ? chat.title : values.text.replace(/\s+/g, ' ').slice(0, 90), updatedAt: Date.now() })
      if (answer)
        this.service.questions.save({ ...answer.question, status: 'answering', messageId: values.id })
      return this.store.putChatMessage({
        ...values,
        ...(answer ? { questionId: answer.question.id, answers: answer.answers } : {}),
        chatId: id,
        status: 'queued',
        createdAt: Date.now(),
      })
    })
  }

  edit(id: string, messageId: string, input?: unknown) {
    const chat = this.get(id)
    const message = requireValue(this.store.chatMessages(id).find(message => message.id === messageId))
    if (message.status !== 'queued')
      throw new AppError(409, 'This message is already being sent.')
    if (input === undefined) {
      return this.store.transaction(() => {
        if (message.questionId) {
          const question = this.service.questions.list(id).find(question => question.id === message.questionId)
          if (question)
            this.service.questions.save({ ...question, status: 'pending', messageId: undefined })
        }

        this.store.deleteChatMessage(id, messageId)
        return { deleted: true }
      })
    }

    if (message.questionId)
      throw new AppError(409, 'A submitted answer cannot be edited.')
    const values = chatMessageInput.parse({ ...(input as object), id: messageId })
    this.validateSteer(chat, values)
    return this.store.putChatMessage({ ...message, ...values })
  }

  private validateSteer(chat: Chat, message: { mode: string, model: string }) {
    const run = chat.runId ? this.store.run(chat.runId) : undefined
    if (message.mode === 'steer' && message.model && run && ['queued', 'running'].includes(run.status) && message.model !== run.snapshot.agent.model)
      throw new AppError(409, 'Queue this message to change model on the next turn.')
  }

  pause(id: string, paused: boolean) {
    return this.store.put('chats', { ...this.get(id), paused, updatedAt: Date.now() })
  }

  acknowledge(runId: string, messageId: string) {
    const chat = this.store.list('chats').find(chat => chat.runId === runId)
    if (!chat)
      return
    const message = this.store.chatMessages(chat.id).find(message => message.id === messageId)
    if (!message || message.status === 'delivered')
      return
    this.store.transaction(() => {
      this.store.putChatMessage({ ...message, status: 'delivered' })
      if (message.questionId)
        this.service.questions.acknowledge(chat.id, message.questionId)
      const question = message.questionId ? this.service.questions.list(chat.id).find(question => question.id === message.questionId) : undefined
      const text = question?.fields.some(field => field.secret) ? 'Answered a private question.' : message.text
      this.store.event(runId, 'chat.user', text, { messageId, text })
      this.store.put('chats', { ...chat, updatedAt: Date.now() })
    })
  }

  async tick(worker: Worker) {
    for (const chat of this.store.list('chats')) {
      let run = chat.runId ? this.store.run(chat.runId) : undefined
      const messages = this.store.chatMessages(chat.id)
      if (run && ['queued', 'running'].includes(run.status)) {
        const executionId = run.chatExecution?.messageId
        const steering = messages.filter(message => (!chat.paused || message.questionId) && message.mode === 'steer' && message.status !== 'delivered' && message.id !== executionId)
        const directory = path.join(this.service.config.dataDir, 'runs', run.id, 'chat-input')
        await mkdir(directory, { recursive: true, mode: 0o700 })
        const currentSteering = steering.flatMap((message) => {
          const current = this.store.chatMessages(chat.id).find(item => item.id === message.id)
          if (!current || current.status === 'delivered' || current.mode !== 'steer')
            return []
          this.store.putChatMessage({ ...current, status: 'sending' })
          return [current]
        })
        const file = path.join(directory, 'messages.json')
        await writeFile(`${file}.tmp`, JSON.stringify(currentSteering), { mode: 0o600 })
        await rename(`${file}.tmp`, file)
        continue
      }

      if (chat.paused)
        continue
      if (run && (worker.active.has(run.id) || run.recoveryPending))
        continue
      if (run && run.status !== 'succeeded') {
        this.pause(chat.id, true)
        continue
      }

      for (const pending of messages) {
        if (pending.status === 'sending') {
          pending.status = 'queued'
          this.store.putChatMessage(pending)
        }
      }

      const message = messages.find(message => message.status === 'queued')
      if (!message)
        continue
      const task = {
        ...taskInput.parse({
          name: chat.title,
          prompt: message.text,
          agentId: chat.agentId,
          projectId: chat.projectId,
          worktree: true,
        }),
        id: chat.id,
        createdAt: chat.createdAt,
        nextRun: null,
      }
      try {
        const snapshot = await this.service.snapshotRun(task, 'chat')
        // Reads above may have raced a queue edit or pause while skills loaded.
        const current = this.store.chatMessages(chat.id).find(item => item.id === message.id)
        if (this.get(chat.id).paused || current?.status !== 'queued' || current.text !== message.text || current.model !== message.model)
          continue
        if (message.model)
          snapshot.snapshot.agent = { ...snapshot.snapshot.agent, model: message.model }
        if (run) {
          // Existing workspaces and permissions are a fixed conversation boundary.
          if (JSON.stringify(snapshot.snapshot.agent.access) !== JSON.stringify(run.snapshot.agent.access))
            throw new AppError(409, 'Agent access changed. Start a new chat with the updated permissions.')
          const checkpoint = requireValue(worker.recovery.get(run.id))
          checkpoint.completed = false
          checkpoint.lastMessage = undefined
          checkpoint.remainingMs = snapshot.snapshot.agent.timeoutMinutes > 0 ? snapshot.snapshot.agent.timeoutMinutes * 60000 : null
          delete checkpoint.settled
          this.store.transaction(() => {
            worker.recovery.save(run!.id, checkpoint)
            run = this.store.updateRun(run!.id, {
              snapshot: snapshot.snapshot,
              status: 'queued',
              summary: '',
              finishedAt: null,
              cancelRequestedAt: null,
              recoveryPending: true,
              chatExecution: { messageId: message.id, text: message.text, recovery: false },
            })
            this.store.putChatMessage({ ...message, status: 'sending' })
          })
        }
        else {
          snapshot.chatExecution = { messageId: message.id, text: message.text, recovery: false }
          this.store.transaction(() => {
            run = this.store.addRun(snapshot)
            this.store.put('chats', { ...this.get(chat.id), runId: run.id })
            this.store.putChatMessage({ ...message, status: 'sending' })
          })
        }
      }
      catch (error) {
        this.pause(chat.id, true)
        this.store.set(`chat-error:${chat.id}`, error instanceof AppError ? error.message : 'Unable to prepare this conversation.')
      }
    }
  }
}
