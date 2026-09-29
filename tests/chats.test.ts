import { execFileSync } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import path from 'node:path'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
} from 'vitest'
import { MAIN_AGENT_ID } from '../shared/constants.ts'
import { accountFixture } from './codex-account-fixture.ts'
import { fixture } from './helpers.ts'
import { Worker } from './legacy/server/worker.ts'
import { runnerProvider } from './runner-provider.ts'

describe('interactive chats', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  let restarted: Worker | undefined
  beforeEach(async () => {
    ctx = await fixture({ codexBin: path.resolve('tests/fixtures/codex.mjs') })
    execFileSync('git', ['init', '-b', 'main', ctx.projectPath])
    execFileSync('git', ['-C', ctx.projectPath, '-c', 'user.name=Test', '-c', 'user.email=test@example.test', 'commit', '--allow-empty', '-m', 'Initial'])
  })
  afterEach(async () => {
    await restarted?.close()
    restarted = undefined
    await ctx.dispose()
  })
  const send = (id: string, text: string, mode: 'queue' | 'steer' = 'queue') => ctx.service.chats.send(id, { id: randomUUID(), text, mode })
  const running = async (chatId: string) => {
    await ctx.worker.tick()
    const id = ctx.service.chats.get(chatId).runId!
    await expect.poll(() => {
      const run = ctx.service.store.run(id)
      if (run?.status === 'failed')
        return `${run.summary}: ${JSON.stringify(ctx.service.store.events(id))}`
      return ctx.service.chats.detail(chatId).messages[0]?.status
    }, { timeout: 10000 }).toBe('delivered')
    return id
  }

  const finish = async (id: string) => {
    await expect.poll(() => ctx.service.store.run(id)?.status, { timeout: 10000 }).toBe('succeeded')
    await expect.poll(() => (restarted ?? ctx.worker).active.has(id)).toBe(false)
  }

  it('defaults project chat to Main agent, validates access, and keeps chats out of tasks', () => {
    const chat = ctx.service.chats.create({ projectId: ctx.project.id })
    expect(chat.agentId).toBe(MAIN_AGENT_ID)
    const agent = ctx.service.agent({ name: 'Restricted', access: { projects: [], github: false } })
    expect(() => ctx.service.chats.create({ projectId: ctx.project.id, agentId: agent.id })).toThrow('unavailable')
    expect(ctx.service.store.list('tasks')).toHaveLength(1)
  })
  it('queues, edits, cancels and deduplicates messages through authenticated endpoints', async () => {
    const headers = await ctx.login()
    const chat = ctx.service.chats.create({})
    const input = { id: randomUUID(), text: 'First draft' }
    expect((await ctx.app.inject({ method: 'POST', url: `/api/chats/${chat.id}/messages`, payload: input })).statusCode).toBe(401)
    for (let n = 0; n < 2; n++) {
      expect((await ctx.app.inject({
        method: 'POST',
        url: `/api/chats/${chat.id}/messages`,
        headers,
        payload: input,
      })).statusCode).toBe(200)
    }

    expect(ctx.service.store.chatMessages(chat.id)).toHaveLength(1)
    ctx.service.chats.edit(chat.id, input.id, { text: 'Updated', mode: 'queue' })
    expect(ctx.service.chats.detail(chat.id).messages[0].text).toBe('Updated')
    ctx.service.chats.edit(chat.id, input.id)
    expect(ctx.service.chats.detail(chat.id).messages).toEqual([])
  })
  it('steers a live turn and runs queued follow-ups in the same session and workspace', async () => {
    const chat = ctx.service.chats.create({ projectId: ctx.project.id })
    send(chat.id, 'Inspect the architecture. fixture:chat-hang')
    const id = await running(chat.id)
    const workspace = ctx.service.store.run(id)!.workspace
    const queued = send(chat.id, 'Then add a test plan')
    expect(() => ctx.service.chats.send(chat.id, {
      id: randomUUID(),
      text: 'Switch now',
      mode: 'steer',
      model: 'different-model',
    })).toThrow('next turn')
    expect(() => ctx.service.chats.edit(chat.id, queued.id, { ...queued, mode: 'steer', model: 'different-model' })).toThrow('next turn')
    const steer = send(chat.id, 'Focus on accessibility, finish now', 'steer')
    await ctx.worker.tick()
    await finish(id)
    expect(ctx.service.chats.detail(chat.id).messages.find(message => message.id === steer.id)?.status).toBe('delivered')
    expect(ctx.service.chats.detail(chat.id).messages.find(message => message.id === queued.id)?.status).toBe('queued')
    await ctx.worker.tick()
    await finish(id)
    expect(ctx.service.store.run(id)).toMatchObject({ sessionId: 'fixture-chat', workspace })
    expect(ctx.service.store.chatMessages(chat.id).every(message => message.status === 'delivered')).toBe(true)
    const transcript = JSON.parse(await readFile(path.join(ctx.service.config.dataDir, 'runs', id, 'codex', 'fixture-conversation.json'), 'utf8'))
    expect(transcript.turns).toHaveLength(2)
    expect(transcript.turns[0].items.filter((item: any) => item.type === 'userMessage')).toHaveLength(2)
    expect(ctx.service.store.events(id).filter(event => event.type === 'chat.user')).toHaveLength(3)
  })
  it('preserves the current conversation and queued messages through restart', async () => {
    const chat = ctx.service.chats.create({})
    send(chat.id, 'Explore the code. fixture:chat-hang')
    const id = await running(chat.id)
    send(chat.id, 'Summarize your findings')
    await ctx.worker.close()
    restarted = new Worker(ctx.service)
    await restarted.tick()
    await finish(id)
    await restarted.tick()
    await finish(id)
    expect(ctx.service.store.chatMessages(chat.id).every(message => message.status === 'delivered')).toBe(true)
    expect(ctx.service.store.events(id).filter(event => event.type === 'chat.user')).toHaveLength(2)
  })
  it('reconciles accepted messages after a lost completion notification without another turn', async () => {
    const chat = ctx.service.chats.create({})
    send(chat.id, 'Review the project')
    const id = await running(chat.id)
    await finish(id)
    const checkpoint = ctx.worker.recovery.get(id)!
    checkpoint.completed = false
    ctx.worker.recovery.save(id, checkpoint)
    ctx.service.store.updateRun(id, { status: 'queued', recoveryPending: true })
    await ctx.worker.close()
    restarted = new Worker(ctx.service)
    await restarted.tick()
    await finish(id)
    const transcript = JSON.parse(await readFile(path.join(ctx.service.config.dataDir, 'runs', id, 'codex', 'fixture-conversation.json'), 'utf8'))
    expect(transcript.turns).toHaveLength(1)
    expect(ctx.service.store.events(id).filter(event => event.type === 'chat.user')).toHaveLength(1)
  })

  it('fails promptly on a disconnected Codex process and retains the queued follow-up', async () => {
    const chat = ctx.service.chats.create({})
    send(chat.id, 'fixture:disconnect')
    send(chat.id, 'Keep this follow-up')
    await ctx.worker.tick()
    const id = ctx.service.chats.get(chat.id).runId!
    await expect.poll(() => ctx.service.store.run(id)?.status, { timeout: 10000 }).toBe('failed')
    await expect.poll(() => ctx.worker.active.has(id)).toBe(false)
    await ctx.worker.tick()
    expect(ctx.service.chats.get(chat.id).paused).toBe(true)
    expect(ctx.service.store.chatMessages(chat.id)[1].status).toBe('queued')
  })

  it('runs restricted chats through the isolated runner with a scoped read-only inbox', async () => {
    const accounts = await accountFixture(ctx)
    accounts.seed('Chat account')
    const broker = await runnerProvider(ctx.service.config.dataDir)
    ctx.service.config.runnerUrl = broker.url
    const agent = ctx.service.agent({
      name: 'Restricted chat',
      access: {
        projects: [ctx.project.id],
        github: false,
        sandbox: 'read-only',
        skills: [],
        mcps: [],
      },
    })
    try {
      const chat = ctx.service.chats.create({ agentId: agent.id, projectId: ctx.project.id })
      send(chat.id, 'Review this workspace')
      const id = await running(chat.id)
      await finish(id)
      expect(ctx.service.store.run(id)?.isolated).toBe(true)
      expect(broker.attempts.size).toBe(1)
      expect(ctx.worker.recovery.get(id)?.prepared?.isolated).toBe(true)
    }
    finally { await broker.close() }
  })

  it('pauses queue dispatch and applies a model override only on the next turn', async () => {
    const chat = ctx.service.chats.create({})
    ctx.service.chats.pause(chat.id, true)
    ctx.service.chats.send(chat.id, { id: randomUUID(), text: 'Review this', model: 'test-model' })
    await ctx.worker.tick()
    expect(ctx.service.chats.get(chat.id).runId).toBeNull()
    ctx.service.chats.pause(chat.id, false)
    const id = await running(chat.id)
    await finish(id)
    expect(ctx.service.store.run(id)?.snapshot.agent.model).toBe('test-model')
    expect(ctx.service.store.get('agents', MAIN_AGENT_ID)?.model).toBe('')
  })
  it.each(['fixture:question', 'fixture:question blocking'])('answers native questions while the same turn is running: %s', async (prompt) => {
    const chat = ctx.service.chats.create({})
    send(chat.id, prompt)
    const runId = await running(chat.id)
    await expect.poll(() => ctx.service.questions.list(chat.id).length).toBe(1)
    const question = ctx.service.questions.list(chat.id)[0]
    expect(question.blocking).toBe(prompt.includes('blocking'))
    expect(ctx.service.store.run(runId)?.status).toBe('running')
    // A paused follow-up queue must not prevent an answer to a waiting agent.
    ctx.service.chats.pause(chat.id, true)
    const answer = { id: randomUUID(), answers: { direction: ['Gradual rollout (Recommended)'] } }
    ctx.service.questions.answer(chat.id, question.id, answer)
    ctx.service.questions.answer(chat.id, question.id, answer)
    expect(() => ctx.service.questions.answer(chat.id, question.id, { ...answer, id: randomUUID() })).toThrow('already been answered')
    await ctx.worker.tick()
    await finish(runId)
    expect(ctx.service.questions.list(chat.id)[0].status).toBe('answered')
    const transcript = JSON.parse(await readFile(path.join(ctx.service.config.dataDir, 'runs', runId, 'codex', 'fixture-conversation.json'), 'utf8'))
    expect(transcript.turns).toHaveLength(1)
    expect(transcript.turns[0].items.find((item: any) => item.type === 'fixtureAnswer').answers.direction.answers).toEqual(answer.answers.direction)
    expect(ctx.service.store.events(runId).filter(event => event.type === 'chat.user')).toHaveLength(2)
  })

  it('keeps expired questions answerable as a follow-up without reusing a stale RPC id', async () => {
    const chat = ctx.service.chats.create({})
    send(chat.id, 'fixture:question expire')
    const runId = await running(chat.id)
    await finish(runId)
    const question = ctx.service.questions.list(chat.id)[0]
    expect(question).toMatchObject({ status: 'pending', blocking: false })
    const input = { id: randomUUID(), answers: { direction: ['Keep the current layout for now.'] } }
    const headers = await ctx.login()
    expect((await ctx.app.inject({ method: 'POST', url: `/api/chats/${chat.id}/questions/${question.id}/answer`, payload: input })).statusCode).toBe(401)
    expect((await ctx.app.inject({
      method: 'POST',
      url: `/api/chats/${chat.id}/questions/${question.id}/answer`,
      headers,
      payload: { ...input, answers: {} },
    })).statusCode).toBe(400)
    expect((await ctx.app.inject({
      method: 'POST',
      url: `/api/chats/${chat.id}/questions/${question.id}/answer`,
      headers,
      payload: input,
    })).statusCode).toBe(200)
    await ctx.worker.tick()
    await finish(runId)
    expect(ctx.service.questions.list(chat.id)[0].status).toBe('answered')
  })

  it('persists async questions, deduplicates replay and answers them after a restart', async () => {
    const chat = ctx.service.chats.create({})
    send(chat.id, 'fixture:async-question')
    const runId = await running(chat.id)
    await finish(runId)
    const question = ctx.service.questions.list(chat.id)[0]
    expect(question.fields[0].title).toBe('Which layout would you prefer?')
    ctx.service.questions.receive(runId, question)
    expect(ctx.service.questions.list(chat.id)).toHaveLength(1)
    await ctx.worker.close()
    restarted = new Worker(ctx.service)
    ctx.service.questions.answer(chat.id, question.id, { id: randomUUID(), answers: { 0: ['Split view (Recommended)'] } })
    await restarted.tick()
    await finish(runId)
    expect(ctx.service.chats.detail(chat.id).pendingQuestions).toBe(0)
    expect(ctx.service.questions.list(chat.id)[0].status).toBe('answered')
  })
})
