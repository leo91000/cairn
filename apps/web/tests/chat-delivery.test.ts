import type { ChatDetail, ChatMessage } from '../../../packages/contracts/chats'
import type { Run, RunEvent } from '../../../packages/contracts/contracts'
import { describe, expect, it } from 'vitest'
import { chatDelivery, chatWaitNotice } from '../src/chat-delivery'

function message(id: string, overrides: Partial<ChatMessage> = {}): ChatMessage {
  return {
    id,
    chatId: 'chat',
    text: id,
    status: 'queued',
    mode: 'queue',
    model: '',
    createdAt: 1,
    ...overrides,
  }
}

function chat(messages: ChatMessage[], run: Partial<Run> | null = null, paused = false): ChatDetail {
  return {
    id: 'chat',
    title: 'Chat',
    agentId: 'agent',
    agentName: 'Agent',
    projectId: null,
    projectName: null,
    runId: run?.id ?? null,
    paused,
    createdAt: 1,
    updatedAt: 1,
    status: run?.status ?? 'idle',
    pendingQuestions: 0,
    questions: [],
    messages,
    run: run as Run | null,
  }
}

const running = { id: 'run', status: 'running' as const, chatExecution: { messageId: 'first', text: 'first', recovery: false } }

describe('chat delivery presentation', () => {
  it('explains a required Claude reconnection without claiming the queued agent has started', () => {
    const detail = chat([message('first', { status: 'sending' })], {
      ...running,
      status: 'queued',
      accountWaitReason: 'Reconnect your Claude Code account in Connections to continue.',
      accountRequired: 'claude',
    })
    expect(chatWaitNotice(detail.run)).toEqual({ account: 'Claude Code', message: 'Reconnect your Claude Code account in Connections to continue. Your message is saved and will be sent then.' })
    expect(chatDelivery(detail, []).sending[0].label).toBe('Waiting for a Claude Code account')
    detail.run!.status = 'running'
    expect(chatWaitNotice(detail.run)).toBeNull()
    expect(chatDelivery(detail, []).sending[0].label).toBe('Starting agent…')
  })
  it('preserves other waiting reasons without offering an unrelated reconnection', () => {
    const detail = chat([message('first')], { ...running, status: 'queued', accountWaitReason: 'Waiting for the previous execution to stop before recovery.' })
    expect(chatWaitNotice(detail.run)).toEqual({ account: null, message: detail.run!.accountWaitReason })
    expect(chatDelivery(detail, []).sending[0].label).toBe('Waiting for the agent…')
    expect(chatWaitNotice({ ...detail.run!, accountWaitReason: '' })).toBeNull()
  })
  it('puts the first idle send in the transcript while later messages wait', () => {
    const result = chatDelivery(chat([message('first'), message('next')]), [])
    expect(result.sending.map(item => [item.message.id, item.label])).toEqual([['first', 'Sending…']])
    expect(result.queued.map(item => item.id)).toEqual(['next'])
  })
  it('keeps a dispatched initial message out of the queue while starting the agent', () => {
    const result = chatDelivery(chat([message('first', { status: 'sending' }), message('next')], running), [])
    expect(result.sending[0].label).toBe('Starting agent…')
    expect(result.queued.map(item => item.id)).toEqual(['next'])
  })
  it('keeps normal follow-ups queued but shows steering delivery in the transcript', () => {
    const result = chatDelivery(chat([message('first', { status: 'delivered' }), message('next'), message('steer', { mode: 'steer', status: 'sending' })], running), [])
    expect(result.sending.map(item => [item.message.id, item.label])).toEqual([['steer', 'Sending to agent…']])
    expect(result.queued.map(item => item.id)).toEqual(['next'])
  })
  it('does not present paused or failed-chat messages as an ongoing send', () => {
    for (const detail of [chat([message('next')], null, true), chat([message('next')], { status: 'failed' })]) {
      expect(chatDelivery(detail, []).sending).toEqual([])
      expect(chatDelivery(detail, []).queued).toHaveLength(1)
    }
  })
  it('moves the next waiting follow-up into the transcript when the previous reply finishes', () => {
    expect(chatDelivery(chat([message('next')], { status: 'succeeded' }), []).sending[0].message.id).toBe('next')
  })
  it('shows an in-flight request before the server snapshot arrives and reconciles by id', () => {
    const outgoing = message('first')
    expect(chatDelivery(null, [], outgoing).sending).toHaveLength(1)
    expect(chatDelivery(chat([message('first', { status: 'sending' })], running), [], outgoing).sending).toHaveLength(1)
    const events: RunEvent[] = [{
      id: 1,
      runId: 'run',
      type: 'chat.user',
      text: 'first',
      createdAt: 1,
      payload: { messageId: 'first' },
    }]
    // Either arrival order (event or metadata first) keeps a single visible message.
    expect(chatDelivery(chat([message('first', { status: 'sending' })], running), events, outgoing).sending).toEqual([])
    expect(chatDelivery(chat([message('first', { status: 'delivered' })], running), [], outgoing).sending).toHaveLength(1)
  })
  it('respects a server-side queue decision if another client started a reply first', () => {
    const outgoing = message('next')
    const result = chatDelivery(chat([outgoing], running), [], outgoing)
    expect(result.sending).toEqual([])
    expect(result.queued).toHaveLength(1)
  })
  it('does not skip an earlier pending question answer when choosing the next send', () => {
    const answer = message('answer', { questionId: 'question' })
    const result = chatDelivery(chat([answer, message('next')]), [])
    expect(result.sending).toEqual([])
    expect(result.queued.map(item => item.id)).toEqual(['next'])
  })
  it('keeps a paused answer waiting with its queue actions', () => {
    const answer = message('answer', { questionId: 'question', mode: 'steer' })
    const result = chatDelivery(chat([answer], null, true), [])
    expect(result.sending).toEqual([])
    expect(result.queued.map(item => item.id)).toEqual(['answer'])
  })
  it('leaves question answers in their dedicated UI, including private answers', () => {
    const answer = message('answer', {
      questionId: 'private',
      text: 'sensitive answer',
      mode: 'steer',
      status: 'sending',
    })
    expect(chatDelivery(chat([answer], running), [])).toEqual({ sending: [], queued: [] })
  })
})
