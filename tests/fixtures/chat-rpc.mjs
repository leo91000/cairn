import { randomUUID } from 'node:crypto'
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import activityEvents from './activity-events.json' with { type: 'json' }
import { askCairn, navigationQuestions } from './cairn-questions.mjs'

export function chatFixture() {
  const file = path.join(process.env.CODEX_HOME, 'fixture-conversation.json')
  // The thread's model: the requested one, else one the fixture's configuration sets.
  const configModel = path.join(process.env.CODEX_HOME, 'fixture-config-model')
  const threadModel = params => params.model || (existsSync(configModel) ? readFileSync(configModel, 'utf8').trim() : 'fixture-fast')
  const emit = value => process.stdout.write(`${JSON.stringify(value)}\n`)
  const notify = (method, params) => emit({ method, params })
  let thread
  let active
  let counter = 0
  let hold = false
  let questionRequest
  let asked = false
  let resumed = false
  let completionText
  // Like Codex, `last` is the latest request (the active context) and `total` the whole thread.
  const tokenUsage = (turnId, last, total) => notify('thread/tokenUsage/updated', {
    threadId: thread.id,
    turnId,
    tokenUsage: { last, total, modelContextWindow: 258400 },
  })
  const usage = (input, cached, output) => ({
    inputTokens: input,
    cachedInputTokens: cached,
    cacheWriteInputTokens: 0,
    outputTokens: output,
    reasoningOutputTokens: 0,
    totalTokens: input + output,
  })
  const save = () => {
    mkdirSync(process.env.CODEX_HOME, { recursive: true })
    writeFileSync(file, JSON.stringify(thread))
  }

  const finish = () => {
    if (!active)
      return
    const scheduledBrief = thread.fixtureInstructions.includes('This unattended task cannot answer clarification questions')
      && active.items.some(item => item.type === 'userMessage' && item.content[0].text.startsWith('Task brief:\n'))
    const text = completionText || (scheduledBrief
      ? '# Complete\nThe fixture task passed.'
      : `I reviewed the workspace. **The approach looks good.**\n\n${active.items.filter(item => item.type === 'userMessage').map(item => item.content[0].text).join('\n\n')}\n\nReady for the next step.`)
    const item = { id: `reply-${thread.fixtureGeneration || 'legacy'}-${active.id}`, type: 'agentMessage', text }
    active.items.push(item)
    active.status = 'completed'
    save()
    notify('item/completed', { threadId: thread.id, item })
    const turns = thread.turns.length
    tokenUsage(active.id, usage(40000 * turns, 36000 * turns, 900), usage(90000 * turns, 70000 * turns, 3000 * turns))
    notify('turn/completed', { threadId: thread.id, turn: active })
    active = null
  }

  // `thread/compact/start` replaces any running turn with a compaction turn.
  const compact = (request) => {
    if (active) {
      active.status = 'interrupted'
      notify('turn/completed', { threadId: thread.id, turn: active })
    }

    const turn = { id: `turn-${++counter}`, status: 'inProgress', items: [] }
    thread.turns.push(turn)
    emit({ id: request.id, result: {} })
    notify('turn/started', { threadId: thread.id, turn })
    const item = { id: `compaction-${turn.id}`, type: 'contextCompaction' }
    notify('item/started', { threadId: thread.id, turnId: turn.id, item })
    setTimeout(() => {
      turn.items.push(item)
      turn.status = existsSync(path.join(process.env.CODEX_HOME, 'fixture-compaction-fails')) ? 'failed' : 'completed'
      if (turn.status === 'completed') {
        notify('item/completed', { threadId: thread.id, turnId: turn.id, item })
        tokenUsage(turn.id, usage(0, 0, 0), usage(90000, 70000, 3000))
        tokenUsage(turn.id, { ...usage(0, 0, 0), totalTokens: 6200 }, usage(90000, 70000, 3000))
      }
      else {
        turn.error = { message: 'Fixture compaction failed' }
      }

      save()
      notify('turn/completed', { threadId: thread.id, turn })
    }, 200)
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
      active.items.push({
        id: 'answer',
        type: 'fixtureAnswer',
        answers: request.result?.answers,
        error: request.error,
      })
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

    if (method === 'thread/compact/start') {
      appendFileSync(path.join(process.env.CODEX_HOME, 'fixture-compactions.jsonl'), `${JSON.stringify(params)}\n`)
      compact(request)
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
        fixtureInstructions: params.developerInstructions || '',
      }
      save()
      emit({ id: request.id, result: { thread, model: threadModel(params) } })
    }

    if (method === 'thread/resume') {
      resumed = true
      if (!existsSync(file))
        throw new Error('No conversation')
      thread = JSON.parse(readFileSync(file, 'utf8'))
      thread.fixtureConfig = params.config
      thread.fixtureInstructions = params.developerInstructions || ''
      counter = thread.turns.length
      emit({ id: request.id, result: { thread: { ...thread, turns: [] }, model: threadModel(params) } })
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
        hold = (params.input[0].text.includes('fixture:chat-hang') || params.input[0].text.includes('fixture:hang')) && !params.input[0].text.startsWith('Continue the interrupted')
        active = {
          id: `turn-${++counter}`,
          status: 'inProgress',
          items: [],
          model: params.model,
          effort: params.effort,
          serviceTier: params.serviceTier,
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

      const marker = path.join(process.env.CODEX_HOME, 'fixture-restart.json')
      if (text.includes('fixture:restart') || (resumed && existsSync(marker))) {
        if (resumed && existsSync(marker)) {
          const previous = JSON.parse(readFileSync(marker, 'utf8'))
          if (previous.cwd !== process.cwd() || readFileSync(path.join(process.cwd(), 'restart-work.txt'), 'utf8') !== 'preserved before restart')
            throw new Error('Restart lost workspace')
          completionText = '# Resumed\nThe saved conversation and work survived the restart.'
        }
        else {
          writeFileSync(marker, JSON.stringify({ cwd: process.cwd() }))
          writeFileSync(path.join(process.cwd(), 'restart-work.txt'), 'preserved before restart')
          hold = true
        }
      }

      if (text.includes('fixture:activity')) {
        for (const event of activityEvents.filter(event => event.type.startsWith('item.'))) {
          const { item } = event
          if (item.type === 'todo_list') {
            notify('turn/plan/updated', { threadId: thread.id, turnId: active.id, plan: item.items.map(step => ({ step: step.text, status: step.completed ? 'completed' : 'pending' })) })
            continue
          }

          const types = {
            agent_message: 'agentMessage',
            command_execution: 'commandExecution',
            web_search: 'webSearch',
            file_change: 'fileChange',
            mcp_tool_call: 'mcpToolCall',
          }
          const native = {
            ...item,
            type: types[item.type],
            aggregatedOutput: item.aggregated_output,
            exitCode: item.exit_code,
          }
          active.items.push(native)
          notify(event.type === 'item.started' ? 'item/started' : 'item/completed', { threadId: thread.id, item: native })
        }
      }

      if (text.includes('fixture:fail')) {
        active.status = 'failed'
        active.error = { message: 'The fixture task failed.' }
        save()
        notify('turn/completed', { threadId: thread.id, turn: active })
        active = null
        return true
      }

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

      if (text.includes('fixture:verbose-mcp')) {
        const body = 'PAGE_CONTENT_MARKER_'.padEnd(350_000, 'x')
        for (let index = 0; index < 160; index++) {
          const item = {
            id: `history-page-${index}`,
            type: 'mcpToolCall',
            server: 'cairn_installation',
            tool: 'get_run',
            arguments: { runId: 'fixture-audit', after: index },
            status: 'completed',
            result: { content: [{ type: 'text', text: body }], structuredContent: { body }, isError: index === 159 },
          }
          // Keep one native result to prove UI compaction leaves it intact.
          if (index === 0)
            active.items.push(item)
          notify('item/completed', { threadId: thread.id, item })
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

      if (text.includes('fixture:cairn-question')) {
        hold = true
        askCairn(thread.fixtureConfig.mcp_servers.cairn_workspace, navigationQuestions).then(() => {
          hold = false
          finish()
        }).catch((error) => {
          process.stderr.write(`${error.message}\n`)
          process.exit(1)
        })
        return true
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

      // Codex replaces the whole plan of the turn with each update. `fixture:plan-working`
      // leaves the second step in progress while the turn stays open.
      if (text.includes('fixture:plan')) {
        const plan = (explanation, steps) => notify('turn/plan/updated', {
          threadId: thread.id,
          turnId: active.id,
          explanation,
          plan: steps.map(([step, status]) => ({ step, status })),
        })
        plan('Data model first, then the middleware.', [
          ['Explore the auth middleware', 'inProgress'],
          ['Add the session expiry migration', 'pending'],
          ['Refresh tokens in the middleware', 'pending'],
        ])
        notify('item/completed', {
          threadId: thread.id,
          item: {
            id: `plan-read-${active.id}`,
            type: 'commandExecution',
            command: 'rg -n session src',
            status: 'completed',
            exitCode: 0,
            aggregatedOutput: 'src/auth.rs:1',
          },
        })
        plan(null, [
          ['Explore the auth middleware', 'completed'],
          ['Add the session expiry migration', 'inProgress'],
          ['Refresh tokens in the middleware', 'pending'],
          ['Run the checks', 'pending'],
        ])
        if (text.includes('fixture:plan-working')) {
          hold = true
        }
        else {
          plan(null, [
            ['Explore the auth middleware', 'completed'],
            ['Add the session expiry migration', 'completed'],
            ['Refresh tokens in the middleware', 'completed'],
            ['Run the checks', 'completed'],
          ])
        }
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
