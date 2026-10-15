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

    if (method === 'thread/unsubscribe') {
      if (active)
        throw new Error('Cannot release an active thread')
      emit({ id: request.id, result: { status: 'unsubscribed' } })
      thread = undefined
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
        fixtureConfig: params.config,
      }
      save()
      emit({ id: request.id, result: { thread } })
    }

    if (method === 'thread/resume') {
      if (!existsSync(file))
        throw new Error('No conversation')
      thread = JSON.parse(readFileSync(file, 'utf8'))
      thread.fixtureConfig = params.config
      counter = thread.turns.length
      emit({ id: request.id, result: { thread: { ...thread, turns: [] } } })
    }

    if (method === 'thread/turns/list' || method === 'thread/items/list') {
      let data = method === 'thread/turns/list'
        ? thread.turns.filter(turn => !thread.fixtureUnlistedTurns?.includes(turn.id)).map(turn => ({ ...turn, items: params.itemsView === 'notLoaded' ? [] : params.itemsView === 'summary' ? [turn.items.find(item => item.type === 'userMessage'), turn.items.findLast(item => item.type === 'agentMessage')].filter(Boolean) : turn.items }))
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
      if (text.includes('fixture:codex-reconnect')) {
        for (const scope of [
          { threadId: 'another-thread', turnId: active.id, willRetry: true },
          { threadId: thread.id, turnId: 'previous-turn', willRetry: true },
          { threadId: thread.id, turnId: active.id, willRetry: false },
        ]) {
          notify('error', { ...scope, error: { message: 'Ignored retry notification' } })
        }

        notify('error', {
          threadId: thread.id,
          turnId: active.id,
          willRetry: true,
          error: { message: 'Reconnecting... 2/5', codexErrorInfo: 'other' },
        })
      }

      const failures = text.match(/fixture:agent-failures:(.+)$/m)
      if (failures && !thread.fixtureFailures)
        thread.fixtureFailures = JSON.parse(failures[1])
      if (thread.fixtureFailures) {
        const marker = path.join(process.cwd(), 'fixture-retry-work.json')
        const previous = existsSync(marker) ? JSON.parse(readFileSync(marker, 'utf8')) : null
        if (previous && previous.generation !== thread.fixtureGeneration)
          throw new Error('Retry replaced the saved conversation')
        thread.fixtureAttempts = (thread.fixtureAttempts || 0) + 1
        writeFileSync(marker, JSON.stringify({ generation: thread.fixtureGeneration, attempts: thread.fixtureAttempts, cwd: process.cwd() }))
        const failure = thread.fixtureFailures[thread.fixtureAttempts - 1]
        if (failure) {
          active.status = 'failed'
          active.error = failure
          save()
          notify('turn/completed', { threadId: thread.id, turn: active })
          active = null
          return true
        }
      }

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

      if (text.includes('fixture:subagents')) {
        // Codex names the spawned thread once the spawn completes, then reports that thread's
        // own activity and the statuses its collaboration tools observe.
        const child = 'fixture-child'
        const collab = (id, tool, status, fields) => ({
          id,
          type: 'collabAgentToolCall',
          tool,
          status,
          senderThreadId: thread.id,
          receiverThreadIds: [child],
          prompt: null,
          model: null,
          reasoningEffort: null,
          agentsStates: {},
          ...fields,
        })
        const spawn = collab(`spawn-${active.id}`, 'spawnAgent', 'inProgress', { receiverThreadIds: [], prompt: 'Review the keyboard handling\nCheck every shortcut.', model: 'gpt-fixture-mini' })
        notify('item/started', { threadId: thread.id, item: spawn })
        if (text.includes('fixture:subagents-lifecycle')) {
          notify('item/completed', {
            threadId: thread.id,
            item: {
              id: 'early-activity',
              type: 'subAgentActivity',
              kind: 'started',
              agentThreadId: child,
              agentPath: '/root/keyboard',
              model: 'gpt-fixture-mini',
            },
          })
          notify('item/completed', {
            threadId: child,
            item: {
              id: 'early-command',
              type: 'commandExecution',
              command: 'pwd',
              status: 'completed',
              exitCode: 0,
            },
          })
        }

        const spawned = {
          ...spawn,
          status: 'completed',
          receiverThreadIds: [child],
          agentsStates: { [child]: { status: 'pendingInit', message: null } },
        }
        active.items.push(spawned)
        notify('item/completed', { threadId: thread.id, item: spawned })
        if (text.includes('fixture:subagents-message-parent')) {
          const command = {
            id: 'parent-command',
            type: 'commandExecution',
            command: 'pnpm check',
            status: 'inProgress',
          }
          notify('item/started', { threadId: thread.id, turnId: active.id, item: command })
          notify('item/completed', {
            threadId: text.includes('fixture:subagents-message-parent-direct') ? thread.id : child,
            turnId: 'child-turn',
            item: {
              id: 'message-parent',
              type: 'subAgentActivity',
              kind: 'interacted',
              agentThreadId: thread.id,
              agentPath: '/root',
            },
          })
          notify('item/completed', { threadId: thread.id, turnId: active.id, item: { ...command, status: 'completed', exitCode: 0 } })
        }

        const childTurn = { id: 'child-turn', status: 'inProgress', items: [] }
        notify('turn/started', { threadId: child, turn: childTurn })
        if (text.includes('fixture:subagents-lifecycle')) {
          const nested = {
            ...spawn,
            id: 'nested-spawn',
            senderThreadId: child,
            status: 'completed',
            receiverThreadIds: ['fixture-grandchild'],
            agentsStates: { 'fixture-grandchild': { status: 'running', message: null } },
          }
          notify('item/completed', { threadId: child, item: nested })
          notify('item/completed', {
            threadId: 'fixture-grandchild',
            item: {
              id: 'child-command',
              type: 'commandExecution',
              command: 'pnpm test',
              status: 'completed',
              exitCode: 0,
            },
          })
          notify('turn/completed', { threadId: 'fixture-grandchild', turn: { id: 'nested-turn', status: 'completed' } })
          const interrupted = {
            ...spawn,
            id: 'interrupted-spawn',
            status: 'interrupted',
            receiverThreadIds: [],
          }
          notify('item/completed', { threadId: thread.id, item: interrupted })
        }

        notify('item/completed', {
          threadId: child,
          item: {
            id: 'child-command',
            type: 'commandExecution',
            command: 'rg -n shortcut src',
            status: 'completed',
            exitCode: 0,
            aggregatedOutput: 'src/shortcuts.ts:1',
            durationMs: 9,
          },
        })
        // `fixture:subagents-working` leaves the subagent at work while the turn stays open.
        if (text.includes('fixture:subagents-working')) {
          if (text.includes('fixture:subagents-working-team')) {
            const second = collab(`second-${active.id}`, 'spawnAgent', 'completed', {
              receiverThreadIds: ['fixture-second'],
              prompt: 'Check the panel accessibility',
              agentsStates: { 'fixture-second': { status: 'running', message: null } },
            })
            active.items.push(second)
            notify('item/completed', { threadId: thread.id, item: second })
          }

          hold = true
        }
        else {
          notify('item/started', { threadId: child, item: { id: 'child-reply', type: 'agentMessage', text: '' } })
          notify('item/agentMessage/delta', { threadId: child, itemId: 'child-reply', delta: 'Keyboard handling looks right.' })
          notify('item/completed', { threadId: child, item: { id: 'child-reply', type: 'agentMessage', text: 'Keyboard handling looks right.' } })
          const failed = text.includes('fixture:subagents-errored')
          notify('turn/completed', { threadId: child, turn: { ...childTurn, status: failed ? 'failed' : 'completed' } })
          if (failed) {
            notify('item/completed', {
              threadId: thread.id,
              item: {
                id: 'completion',
                type: 'subAgentActivity',
                kind: 'completed',
                agentThreadId: child,
                agentPath: '/root/keyboard',
              },
            })
          }

          if (text.includes('fixture:subagents-lifecycle')) {
            notify('item/completed', {
              threadId: thread.id,
              item: {
                id: 'interaction',
                type: 'subAgentActivity',
                kind: 'interacted',
                agentThreadId: child,
                agentPath: '/root/keyboard',
              },
            })
          }

          const waited = collab(`wait-${active.id}`, 'wait', 'completed', { agentsStates: { [child]: { status: failed ? 'errored' : 'completed', message: 'Keyboard handling looks right.' } } })
          const closed = collab(`close-${active.id}`, 'closeAgent', 'completed', { agentsStates: { [child]: { status: 'shutdown', message: null } } })
          for (const item of [waited, closed]) {
            active.items.push(item)
            notify('item/completed', { threadId: thread.id, item })
          }

          if (text.includes('fixture:subagents-followup'))
            notify('turn/started', { threadId: child, turn: { id: 'follow-up', status: 'inProgress', items: [] } })
        }

        save()
      }

      if (text.includes('fixture:disconnect'))
        process.exit(1)
      if (text.includes('finish now'))
        finish()
      else if (!hold)
        setTimeout(finish, text.includes('fixture:slow-finish') ? 3000 : 250)
    }

    return true
  }

  return handle
}
