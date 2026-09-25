import { randomUUID } from 'node:crypto'
import {
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'

export function chatFixture() {
  const file = path.join(process.env.CODEX_HOME, 'fixture-conversation.json')
  const emit = value => process.stdout.write(`${JSON.stringify(value)}\n`)
  const notify = (method, params) => emit({ method, params })
  let thread
  let active
  let counter = 0
  let hold = false
  let questionRequest
  let asked = false
  const save = () => {
    mkdirSync(process.env.CODEX_HOME, { recursive: true })
    writeFileSync(file, JSON.stringify(thread))
  }

  const finish = () => {
    if (!active)
      return
    const text = `I reviewed the workspace. **The approach looks good.**\n\n${active.items.filter(item => item.type === 'userMessage').map(item => item.content[0].text).join('\n\n')}\n\nReady for the next step.`
    const item = { id: `reply-${thread.fixtureGeneration || 'legacy'}-${active.id}`, type: 'agentMessage', text }
    active.items.push(item)
    active.status = 'completed'
    save()
    notify('item/completed', { threadId: thread.id, item })
    notify('turn/completed', { threadId: thread.id, turn: active })
    active = null
  }

  const handle = (request) => {
    const { method, params } = request
    if (!method && request.id === 'auth-refresh') {
      if (!request.result?.accessToken?.endsWith('-refreshed') || request.result.chatgptAccountId !== globalThis.fixtureAccountId || request.result.refresh_token)
        throw new Error('Invalid external token refresh')
      finish()
      return true
    }

    if (!method && questionRequest && request.id === questionRequest) {
      active.items.push({ id: 'answer', type: 'fixtureAnswer', answers: request.result.answers })
      save()
      notify('serverRequest/resolved', { threadId: thread.id, requestId: questionRequest })
      questionRequest = undefined
      finish()
      return true
    }

    if (!['thread/start', 'thread/resume', 'turn/start', 'turn/steer', 'thread/turns/list', 'thread/items/list'].includes(method))
      return false
    if (method === 'thread/start') {
      thread = {
        fixtureGeneration: randomUUID(),
        id: 'fixture-chat',
        cwd: params.cwd,
        historyMode: 'paginated',
        turns: [],
        parentThreadId: null,
      }
      save()
      emit({ id: request.id, result: { thread } })
    }

    if (method === 'thread/resume') {
      if (!existsSync(file))
        throw new Error('No conversation')
      thread = JSON.parse(readFileSync(file, 'utf8'))
      counter = thread.turns.length
      emit({ id: request.id, result: { thread: { ...thread, turns: [] } } })
    }

    if (method === 'thread/turns/list' || method === 'thread/items/list') {
      let data = method === 'thread/turns/list'
        ? thread.turns.map(turn => ({ ...turn, items: params.itemsView === 'notLoaded' ? [] : params.itemsView === 'summary' ? [turn.items.find(item => item.type === 'userMessage'), turn.items.findLast(item => item.type === 'agentMessage')].filter(Boolean) : turn.items }))
        : thread.turns.filter(turn => !params.turnId || turn.id === params.turnId).flatMap(turn => turn.items.map(item => ({ turnId: turn.id, item })))
      if (params.sortDirection === 'desc')
        data = data.toReversed()
      const start = Number(params.cursor || 0)
      const end = start + (params.limit || 50)
      emit({ id: request.id, result: { data: data.slice(start, end), nextCursor: end < data.length ? `${end}` : null } })
    }

    if (method === 'turn/start' || method === 'turn/steer') {
      if (params.input[0].text.includes('fixture:slow-delivery') && !request.delayed) {
        setTimeout(handle, 8000, { ...request, delayed: true })
        return true
      }

      if (method === 'turn/steer' && (!active || active.id !== params.expectedTurnId)) {
        emit({ id: request.id, error: { code: -32000, message: 'Turn has finished' } })
        return true
      }

      if (method === 'turn/start') {
        hold = params.input[0].text.includes('fixture:chat-hang') && !params.input[0].text.startsWith('Continue the interrupted')
        active = {
          id: `turn-${++counter}`,
          status: 'inProgress',
          items: [],
          model: params.model,
          effort: params.effort,
        }
        thread.turns.push(active)
        notify('turn/started', { threadId: thread.id, turn: active })
      }

      const item = {
        id: `user-${active.items.length}`,
        clientId: params.clientUserMessageId,
        type: 'userMessage',
        content: params.input,
      }
      active.items.push(item)
      save()
      notify('item/completed', { threadId: thread.id, item })
      emit({ id: request.id, result: { turn: active, turnId: active.id } })
      if (method === 'turn/start') {
        const commentary = { id: `intro-${active.id}`, type: 'agentMessage', text: 'I’ll trace the component boundaries and keyboard handling, then propose a focused change.' }
        active.items.push(commentary)
        notify('item/completed', { threadId: thread.id, item: commentary })
        const command = {
          id: `command-${active.id}`,
          type: 'commandExecution',
          command: 'rg --files src/components',
          status: 'completed',
          exitCode: 0,
          aggregatedOutput: 'ChatComposer.vue\nActivityFeed.vue\nVirtualSelect.vue',
          durationMs: 18,
        }
        active.items.push(command)
        save()
        notify('item/completed', { threadId: thread.id, item: command })
      }

      const text = params.input[0].text
      if (text.includes('fixture:verbose-tools')) {
        for (let index = 0; index < 60; index++) {
          notify('item/completed', {
            threadId: thread.id,
            item: {
              id: `verbose-${index}`,
              type: 'commandExecution',
              command: 'fixture verbose output',
              status: 'completed',
              exitCode: 0,
              aggregatedOutput: 'x'.repeat(100_000),
            },
          })
        }

        if (text.includes('fixture:verbose-tools-fail')) {
          active.status = 'failed'
          active.error = { message: 'Failure after verbose tools' }
          notify('turn/completed', { threadId: thread.id, turn: active })
          active = null
          return true
        }

        notify('item/started', { threadId: thread.id, item: { id: 'after-tools', type: 'agentMessage', text: '' } })
        notify('item/agentMessage/delta', { threadId: thread.id, itemId: 'after-tools', delta: 'Still responding after verbose tools.' })
        notify('item/completed', { threadId: thread.id, item: { id: 'after-tools', type: 'agentMessage', text: 'Still responding after verbose tools.' } })
      }

      if (text.includes('fixture:exhaust') && !existsSync(path.join(process.env.CODEX_HOME, 'fixture-exhausted.json'))) {
        writeFileSync(path.join(process.env.CODEX_HOME, 'fixture-exhausted.json'), JSON.stringify({ account: globalThis.fixtureAccountId, cwd: process.cwd() }))
        writeFileSync(path.join(process.cwd(), 'preserved-work.txt'), 'work before exhaustion')
        active.status = 'failed'
        active.error = { message: 'You\'ve hit your usage limit. Try again later.' }
        save()
        notify('turn/completed', { threadId: thread.id, turn: active })
        active = null
        return true
      }

      if (existsSync(path.join(process.env.CODEX_HOME, 'fixture-exhausted.json'))) {
        const previous = JSON.parse(readFileSync(path.join(process.env.CODEX_HOME, 'fixture-exhausted.json'), 'utf8'))
        if (previous.account === globalThis.fixtureAccountId || previous.cwd !== process.cwd() || readFileSync(path.join(process.cwd(), 'preserved-work.txt'), 'utf8') !== 'work before exhaustion')
          throw new Error('Resume did not preserve context or switch accounts')
      }

      if (!asked && text.includes('fixture:question')) {
        asked = true
        hold = true
        questionRequest = 'question-1'
        emit({
          id: questionRequest,
          method: 'item/tool/requestUserInput',
          params: {
            threadId: thread.id,
            turnId: active.id,
            itemId: 'question-item',
            isBlocking: text.includes('blocking'),
            questions: [{
              id: 'direction',
              header: 'Direction',
              question: 'How should we introduce the new navigation?',
              isOther: true,
              isSecret: false,
              options: [{ label: 'Gradual rollout (Recommended)', description: 'Start with the chat view, then extend it to the rest of the app.' }, { label: 'Update everything', description: 'Refresh all views together in one release.' }],
            }],
          },
        })
        if (text.includes('expire')) {
          notify('serverRequest/resolved', { threadId: thread.id, requestId: questionRequest })
          questionRequest = undefined
          finish()
        }
      }

      if (text.includes('fixture:async-question')) {
        const question = {
          id: 'async-question-item',
          type: 'agentMessage',
          text: 'I’ll use the existing violet palette while you consider the layout.',
          questions: [{ title: 'Which layout would you prefer?', options: ['Split view (Recommended)', 'Full-width conversation'] }],
        }
        active.items.push(question)
        save()
        notify('item/completed', { threadId: thread.id, item: question })
      }

      if (text.includes('fixture:auth-refresh')) {
        emit({ id: 'auth-refresh', method: 'account/chatgptAuthTokens/refresh', params: { reason: 'unauthorized', previousAccountId: globalThis.fixtureAccountId } })
        return true
      }

      if (text.includes('fixture:stream')) {
        const streaming = active
        const item = { id: `stream-${active.id}`, type: 'agentMessage', text: 'Streaming proof:' }
        active.items.push(item)
        notify('item/started', { threadId: thread.id, item: { ...item, text: '' } })
        notify('item/agentMessage/delta', { threadId: thread.id, itemId: item.id, delta: item.text })
        let part = 0
        const timer = setInterval(() => {
          if (active !== streaming) {
            clearInterval(timer)
            return
          }

          const delta = ` ${String(++part).padStart(3, '0')}`
          item.text += delta
          save()
          notify('item/agentMessage/delta', { threadId: thread.id, itemId: item.id, delta })
          if (part === 100) {
            clearInterval(timer)
            notify('item/completed', { threadId: thread.id, item })
            finish()
          }
        }, 80)
        return true
      }

      if (text.includes('fixture:disconnect'))
        process.exit(1)
      if (text.includes('finish now'))
        finish()
      else if (!hold)
        setTimeout(finish, 250)
    }

    return true
  }

  return handle
}
