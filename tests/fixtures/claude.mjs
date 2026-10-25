#!/usr/bin/env node
// Synthetic official-CLI protocol fixture. Never connects to a model provider.
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'

const home = process.env.CLAUDE_CONFIG_DIR
if (!home)
  throw new Error('Missing isolated fixture home')
mkdirSync(home, { recursive: true })
const auth = path.join(process.env.CLAUDE_SECURESTORAGE_CONFIG_DIR || home, '.credentials.json')
const args = process.argv.slice(2)
const out = value => process.stdout.write(`${JSON.stringify(value)}\n`)
const lines = createInterface({ input: process.stdin })
if (args[0] === 'auth') {
  if (args[1] === 'status') {
    let email = 'claude-fixture@example.test'
    try {
      email = JSON.parse(readFileSync(auth, 'utf8')).fixtureEmail ?? email
    }
    catch {}

    out({
      loggedIn: existsSync(auth),
      email,
      subscriptionType: 'max',
      authMethod: 'oauth_token',
      accessToken: 'never-return-this-secret',
    })
    process.exit(existsSync(auth) ? 0 : 1)
  }

  if (args[1] === 'logout') {
    rmSync(auth, { force: true })
    process.exit(0)
  }

  process.stdout.write('Open https://claude.com/oauth/authorize?fixture=1\nPaste code here if prompted > ')
  lines.on('line', (code) => {
    // `fixture-code:<email>` signs in another synthetic identity.
    const match = /^fixture-code(?::(.+))?$/.exec(code)
    if (!match) {
      process.stderr.write('never-return-this-secret')
      process.exit(1)
    }

    writeFileSync(auth, 'connected')
    writeFileSync(path.join(home, '.credentials.json'), JSON.stringify({
      fixtureEmail: match[1],
      claudeAiOauth: {
        accessToken: 'fixture-access',
        refreshToken: 'fixture-refresh',
        expiresAt: Date.now() + 3600000,
        scopes: ['user:profile', 'user:inference'],
        subscriptionType: 'max',
      },
    }))
    process.exit(0)
  })
}
else {
  if (!existsSync(auth) && !args.includes('--no-session-persistence')) {
    out({
      type: 'result',
      subtype: 'error_during_execution',
      is_error: true,
      result: 'Not logged in',
    })
    process.exit(1)
  }

  const session = '70f5e7a1-8d65-4f5f-a545-af6ee8c0e1ab'
  const log = path.join(home, 'invocations.jsonl')
  const seenFile = path.join(home, 'seen-message-ids.json')
  const seen = new Set(args.includes('--resume') && existsSync(seenFile) ? JSON.parse(readFileSync(seenFile, 'utf8')) : [])
  const instructionsFile = args.indexOf('--append-system-prompt-file')
  const instructions = instructionsFile >= 0
    ? readFileSync(args[instructionsFile + 1], 'utf8')
    : args[args.indexOf('--append-system-prompt') + 1]
  const instructionsMode = instructionsFile >= 0
    ? statSync(args[instructionsFile + 1]).mode & 0o777
    : undefined
  const backgroundTasksDisabled = process.env.CLAUDE_CODE_DISABLE_BACKGROUND_TASKS
  appendFileSync(log, `${JSON.stringify({
    args,
    instructions,
    instructionsMode,
    backgroundTasksDisabled,
  })}\n`)
  out({ type: 'system', subtype: 'init', session_id: session })
  let question
  let serial = 0
  const complete = (text = 'Claude fixture completed', thinking = false, correlation = {}) => {
    const id = `assistant-${process.pid}-${++serial}`
    out({ type: 'stream_event', event: { type: 'message_start', message: { id } } })
    // Like Claude Code, stream real block indexes but emit one assistant event per block.
    if (thinking) {
      out({ type: 'stream_event', event: { type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } } })
      out({ type: 'stream_event', event: { type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'Fixture reasoning' } } })
      out({ type: 'assistant', message: { id, content: [{ type: 'thinking', thinking: 'Fixture reasoning' }] } })
    }

    const index = thinking ? 1 : 0
    out({ type: 'stream_event', event: { type: 'content_block_start', index, content_block: { type: 'text', text: '' } } })
    out({ type: 'stream_event', event: { type: 'content_block_delta', index, delta: { type: 'text_delta', text } } })
    out({ type: 'assistant', message: { id, content: [{ type: 'text', text }] } })
    out({
      type: 'result',
      subtype: 'success',
      is_error: false,
      result: text,
      session_id: session,
      ...correlation,
    })
  }

  lines.on('line', (line) => {
    const value = JSON.parse(line)
    if (value.type === 'user')
      appendFileSync(path.join(home, 'user-messages.jsonl'), `${JSON.stringify(value)}\n`)
    if (value.type === 'control_request') {
      if (value.request.subtype === 'get_usage') {
        appendFileSync(path.join(home, 'usage-requests.jsonl'), `${JSON.stringify(value.request)}\n`)
        const custom = path.join(home, 'fixture-usage.json')
        const response = existsSync(custom)
          ? JSON.parse(readFileSync(custom, 'utf8'))
          : {
              rate_limits_available: true,
              rate_limits: {
                five_hour: { utilization: 25, resets_at: '2030-01-01T12:00:00Z' },
                seven_day: { utilization: 60, resets_at: '2030-01-07T12:00:00Z' },
                seven_day_sonnet: { utilization: 10, resets_at: '2030-01-07T12:00:00Z' },
              },
              accessToken: 'never-return-this-secret',
            }
        if (response.fixtureError)
          out({ type: 'control_response', response: { subtype: 'error', request_id: value.request_id, error: 'never-return-this-secret' } })
        else
          out({ type: 'control_response', response: { subtype: 'success', request_id: value.request_id, response } })
        return
      }

      out({
        type: 'control_response',
        response: {
          subtype: 'success',
          request_id: value.request_id,
          response: {
            models: [{
              value: 'sonnet',
              resolvedModel: 'claude-sonnet-fixture',
              displayName: 'Sonnet',
              description: 'Balanced Claude model',
              supportedEffortLevels: ['low', 'medium', 'high'],
            }, {
              value: 'opus',
              resolvedModel: 'claude-opus-fixture',
              displayName: 'Opus',
              description: 'Deep reasoning',
              supportedEffortLevels: ['low', 'medium', 'high', 'max'],
            }, {
              value: 'default',
              resolvedModel: 'claude-opus-fixture[1m]',
              displayName: 'Default (recommended)',
              description: 'Opus with 1M context · Best for everyday tasks',
              supportedEffortLevels: ['low', 'medium', 'high'],
            }],
          },
        },
      })
      // Like the official CLI, cache the account catalog with each model's default effort just
      // after answering.
      setTimeout(() => {
        mkdirSync(path.join(home, 'cache/model-catalog'), { recursive: true })
        writeFileSync(path.join(home, 'cache/model-catalog/fixture.json'), JSON.stringify({
          version: 2,
          fetchedAt: Date.now(),
          catalog: {
            config: {
              models: [
                { id: 'claude-sonnet-fixture', name: 'Sonnet Fixture', thinking: { effort_options: [{ id: 'low' }, { id: 'medium' }, { id: 'high', badge: { message: 'Default' } }] } },
                { id: 'claude-opus-fixture', name: 'Opus Fixture', thinking: { effort_options: [{ id: 'low' }, { id: 'medium', badge: { message: 'Default' } }, { id: 'high' }, { id: 'max' }] } },
              ],
            },
          },
        }))
      }, 300)
      return
    }

    if (value.type === 'control_response') {
      if (question) {
        writeFileSync(path.join(home, 'question-response.json'), JSON.stringify(value))
        complete('Question answered')
        question = false
      }

      return
    }

    if (value.type !== 'user')
      return
    // The real CLI replays duplicate UUIDs without starting a new model turn.
    if (seen.has(value.uuid)) {
      out(value)
      return
    }

    seen.add(value.uuid)
    writeFileSync(seenFile, JSON.stringify([...seen]))
    appendFileSync(path.join(home, 'inputs.jsonl'), `${JSON.stringify(value)}\n`)
    const prompt = value.message.content.filter(b => b.type === 'text').map(b => b.text).join('\n')
    const correlation = { user_message_uuid: value.uuid }
    if (prompt.includes('fixture:spend-limit') || prompt.includes('Corrige le dialogue de révocation.')) {
      const message = 'You\'ve hit your individual spend limit · run /usage-credits to ask your admin for a higher limit · your weekly limit resets 3pm (UTC)'
      out(value)
      out({ type: 'assistant', message: { id: 'saved-work', content: [{ type: 'text', text: 'Le paramètre details est placé avant action. Les modifications sont conservées ; je poursuis les vérifications.' }] } })
      out({ type: 'assistant', message: { id: 'quota-message', content: [{ type: 'text', text: message }] } })
      out({
        type: 'result',
        subtype: 'error_during_execution',
        is_error: true,
        result: message,
      })
      return
    }

    if (prompt.includes('fixture:resume-dedup') && !args.includes('--resume')) {
      out(value)
      return
    }

    if (prompt.includes('fixture:startup-result')) {
      // Restored background work may finish before the new prompt is consumed.
      out({
        type: 'result',
        subtype: 'success',
        is_error: false,
        result: '',
        origin: { kind: 'task-notification' },
      })
      setTimeout(() => {
        out(value)
        complete('Actual requested response', false, correlation)
      }, 500)
      return
    }

    if (prompt.includes('fixture:result-ack')) {
      complete('Result acknowledges prompt', false, correlation)
      return
    }

    out(value)
    if (prompt.includes('fixture:shell-environment')) {
      const tool = prompt.includes('read-tool') ? 'Read' : prompt.includes('monitor-tool') ? 'Monitor' : 'Bash'
      const id = `shell-${process.pid}`
      const error = prompt.includes('ordinary-error')
        ? 'Command exited with code 1'
        : 'The argument \'args[1]\' must be a string without null bytes. Received "source /home/node/.claude/shell-snapshots/snapshot-bash-fixture.sh && export PRIVATE=never-return-this-secret\\x00"'
      out({
        type: 'assistant',
        message: {
          id: 'shell-call',
          content: [{
            type: 'tool_use',
            id,
            name: tool,
            input: { command: 'pwd' },
          }],
        },
      })
      const content = prompt.includes('array-content') ? [{ type: 'text', text: error }] : error
      out({
        type: 'user',
        message: {
          role: 'user',
          content: [{
            type: 'tool_result',
            tool_use_id: id,
            is_error: !prompt.includes('successful-tool'),
            content,
          }],
        },
      })
      complete('Shell failure incorrectly treated as success', false, correlation)
      return
    }

    // Claude Code rewrites the whole list with each TodoWrite call.
    if (prompt.includes('fixture:plan')) {
      const write = (id, todos) => {
        out({
          type: 'assistant',
          message: {
            id: `plan-${id}`,
            content: [{
              type: 'tool_use',
              id,
              name: 'TodoWrite',
              input: { todos },
            }],
          },
        })
        out({
          type: 'user',
          message: {
            role: 'user',
            content: [{ type: 'tool_result', tool_use_id: id, content: 'Todos have been modified successfully.' }],
          },
        })
      }

      const todo = (content, status, activeForm) => ({ content, status, activeForm })
      write('todo-1', [
        todo('Explore the auth middleware', 'in_progress', 'Exploring the auth middleware'),
        todo('Add the session expiry migration', 'pending', 'Adding the migration'),
      ])
      write('todo-2', [
        todo('Explore the auth middleware', 'completed', 'Exploring the auth middleware'),
        todo('Add the session expiry migration', 'in_progress', 'Adding the migration'),
      ])
      complete('Plan followed', false, correlation)
      return
    }

    if (prompt.includes('fixture:stopped-background')) {
      const snapshotFirst = prompt.includes('snapshot-first')
      const failedStop = prompt.includes('failed-stop')
      const unrelatedTool = prompt.includes('unrelated-tool')
      const tasks = ['first', 'second', 'third'].map(task_id => ({
        task_id,
        task_type: 'local_bash',
        description: `Wait for ${task_id}`,
      }))
      out({ type: 'system', subtype: 'background_tasks_changed', tasks })
      complete('Waiting for background work', false, correlation)
      setTimeout(() => {
        for (const [index, task] of tasks.entries()) {
          const id = `stop-${task.task_id}`
          const last = index === tasks.length - 1
          const args = prompt.includes('legacy-id') ? { shell_id: task.task_id } : { task_id: task.task_id }
          out({
            type: 'assistant',
            message: {
              id,
              content: [{
                type: 'tool_use',
                id,
                name: last && unrelatedTool ? 'Read' : 'TaskStop',
                input: args,
              }],
            },
          })
          // A failed stop can race with the task finishing naturally. Its
          // follow-up still needs to run after the next foreground result.
          const snapshot = { type: 'system', subtype: 'background_tasks_changed', tasks: tasks.slice(index + 1) }
          if (snapshotFirst)
            out(snapshot)
          const stopFailed = last && failedStop
          out({
            type: 'user',
            message: {
              role: 'user',
              content: [{
                type: 'tool_result',
                tool_use_id: id,
                is_error: stopFailed,
                content: JSON.stringify({ message: stopFailed ? 'Task already finished' : `Successfully stopped task: ${task.task_id}`, task_id: task.task_id, task_type: task.task_type }),
              }],
            },
          })
          if (!snapshotFirst)
            out(snapshot)
        }

        // TaskStop does not replay a task-notification user message for these tasks.
        complete('Stopped all background work', false, correlation)
        if (failedStop || unrelatedTool) {
          setTimeout(() => {
            out({
              type: 'user',
              uuid: 'notification',
              origin: { kind: 'task-notification' },
              message: { role: 'user', content: [] },
            })
            complete('Background followup handled', false, { origin: { kind: 'task-notification' } })
          }, 200)
        }
      }, 100)
      return
    }

    if (prompt.includes('fixture:background')) {
      const ambient = prompt.includes('fixture:background-ambient')
      // A slow build leaves time to observe the idle agent waiting for it.
      const buildMs = prompt.includes('fixture:background-slow') ? 8000 : 500
      if (prompt.includes('fixture:background-ambient-flip'))
        out({ type: 'system', subtype: 'background_tasks_changed', tasks: [{ task_id: 'build', task_type: 'local_bash', description: 'Wait for build' }] })
      out({
        type: 'system',
        subtype: 'background_tasks_changed',
        tasks: [{
          task_id: 'build',
          task_type: 'local_bash',
          description: 'Wait for build',
          ambient,
        }],
      })
      complete('Build is still running', false, correlation)
      if (prompt.includes('fixture:background-stream')) {
        setTimeout(() => {
          out({ type: 'stream_event', event: { type: 'message_start', message: { id: 'resumed' } } })
          out({ type: 'stream_event', event: { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } } })
          out({ type: 'stream_event', event: { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'Checking progress' } } })
          out({
            type: 'system',
            subtype: 'background_tasks_changed',
            tasks: [{ task_id: 'build', task_type: 'local_bash', description: 'Build finishing' }],
          })
        }, 100)
      }

      if (!ambient) {
        setTimeout(() => {
          out({ type: 'system', subtype: 'background_tasks_changed', tasks: [] })
          out({
            type: 'system',
            subtype: 'task_notification',
            task_id: 'build',
            status: 'completed',
          })
          // Let the agent consume the notification before declaring the run done.
          setTimeout(() => {
            out({
              type: 'user',
              uuid: 'notification',
              origin: { kind: 'task-notification' },
              message: { role: 'user', content: [] },
            })
            complete('Build checked and task finished', false, { origin: { kind: 'task-notification' } })
          }, 200)
        }, buildMs)
      }

      return
    }

    if (prompt.includes('fixture:engine-retry')) {
      // Claude Code retries API requests itself, and reports the subscription limit and its
      // notices. None of them ends the turn.
      out(value)
      out({ type: 'system', subtype: 'session_state_changed', state: 'running' })
      out({ type: 'rate_limit_event', rate_limit_info: { status: 'allowed', rateLimitType: 'five_hour', resetsAt: 1893499200 } })
      out({
        type: 'system',
        subtype: 'api_retry',
        attempt: 1,
        max_retries: 10,
        retry_delay_ms: 600,
        error_status: 529,
        error: 'overloaded',
      })
      if (prompt.includes('engine-retry-slow')) {
        // Long enough for a reader to see the retry before work continues.
        setTimeout(() => {
          out({
            type: 'system',
            subtype: 'informational',
            level: 'warning',
            content: 'Context is almost full.',
          })
          complete('Answered after the retries', false, correlation)
        }, 5000)
        return
      }

      out({
        type: 'system',
        subtype: 'api_retry',
        attempt: 2,
        max_retries: 1,
        retry_delay_ms: 1200,
        error_status: null,
        error: 'unknown',
        no_response: { waited_ms: 30000, retry_wait_ms: 60000 },
      })
      out({ type: 'system', subtype: 'future_frame', detail: 'ignored by older adapters' })
      out({
        type: 'rate_limit_event',
        rate_limit_info: {
          status: 'allowed_warning',
          rateLimitType: 'five_hour',
          resetsAt: 1893499200,
          utilization: 0.9,
        },
      })
      out({
        type: 'rate_limit_event',
        rate_limit_info: {
          status: 'allowed_warning',
          rateLimitType: 'five_hour',
          resetsAt: 1893499200,
          utilization: 0.92,
        },
      })
      out({
        type: 'system',
        subtype: 'informational',
        level: 'warning',
        content: 'Context is almost full.',
      })
      out({
        type: 'system',
        subtype: 'informational',
        level: 'info',
        content: '   ',
      })
      out({ type: 'rate_limit_event', rate_limit_info: { status: 'allowed', rateLimitType: 'five_hour', resetsAt: 1893517200 } })
      complete('Answered after the retries', false, correlation)
      return
    }

    if (prompt.includes('fixture:usage-limit')) {
      const extra = prompt.includes('extra-usage')
      const reset = prompt.includes('reset-passed')
      // A limit on one model's window: Claude Code answers on another model, then fails for an
      // unrelated reason.
      const answered = prompt.includes('answered')
      out(value)
      out({
        type: 'rate_limit_event',
        rate_limit_info: {
          status: 'rejected',
          rateLimitType: answered ? 'seven_day_opus' : 'five_hour',
          resetsAt: reset ? 1000000000 : 1893499200,
          overageStatus: extra ? 'allowed' : 'rejected',
          isUsingOverage: extra,
        },
      })
      if (extra) {
        complete('Finished on extra usage', false, correlation)
        return
      }

      if (answered) {
        out({ type: 'stream_event', parent_tool_use_id: null, event: { type: 'message_start', message: { id: 'fallback' } } })
        out({
          type: 'result',
          subtype: 'success',
          is_error: true,
          result: 'API Error: 500 Internal server error',
        })
        return
      }

      if (prompt.includes('unrelated')) {
        // The limit refused a subagent's request; the agent's own request fails for another reason.
        const error = 'API Error: 500 Internal server error'
        out({
          type: 'assistant',
          parent_tool_use_id: null,
          error: 'server_error',
          message: { id: 'server-error', content: [{ type: 'text', text: error }] },
        })
        out({
          type: 'result',
          subtype: 'success',
          is_error: true,
          result: error,
        })
        return
      }

      const message = 'Your usage allowance is used up for now.'
      out({ type: 'assistant', error: 'rate_limit', message: { id: 'limit-message', content: [{ type: 'text', text: message }] } })
      out({
        type: 'result',
        subtype: 'success',
        is_error: true,
        result: message,
      })
      return
    }

    if (prompt.includes('fixture:resumed-later')) {
      // A later execution resumes a subagent an earlier one started: this process never saw the
      // tool use that started it. An agent a workflow starts is not one the agent resumed.
      out(value)
      out({
        type: 'assistant',
        parent_tool_use_id: null,
        message: {
          id: 'send',
          content: [{
            type: 'tool_use',
            id: 'send-later',
            name: 'SendMessage',
            input: { to: 'task-old', message: 'Go on.' },
          }],
        },
      })
      for (const seconds of [5, 10]) {
        out({
          type: 'system',
          subtype: 'informational',
          level: 'info',
          tool_use_id: 'send-later',
          content: `Tool SendMessage running for ${seconds}s…`,
        })
      }

      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-old',
        run_id: 'run-3',
        tool_use_id: 'send-later',
        description: 'Review the change',
        subagent_type: 'general-purpose',
        is_backgrounded: true,
        task_type: 'local_agent',
      })
      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-workflow',
        tool_use_id: 'workflow-call',
        description: 'Workflow step',
        task_type: 'local_agent',
      })
      // Nor is a housekeeping agent Claude Code starts itself.
      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-housekeeping',
        description: 'Summarize the session',
        task_type: 'local_agent',
        skip_transcript: true,
        ambient: true,
      })
      out({
        type: 'assistant',
        parent_tool_use_id: null,
        agent_id: 'task-old',
        message: { id: 'old-1', content: [{ type: 'text', text: 'Picked up again.' }] },
      })
      // Before 2.1.292, its messages may name the call that resumed it instead.
      out({
        type: 'assistant',
        parent_tool_use_id: 'send-later',
        message: { id: 'old-2', content: [{ type: 'text', text: 'Still on it.' }] },
      })
      out({
        type: 'system',
        subtype: 'task_notification',
        task_id: 'task-old',
        run_id: 'run-3',
        tool_use_id: 'send-later',
        status: 'completed',
        summary: 'Picked up again.',
        output_file: '',
      })
      out({
        type: 'user',
        parent_tool_use_id: null,
        message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'send-later', content: [{ type: 'text', text: 'Message delivered.' }] }] },
      })
      complete('Resumed in a later turn', false, correlation)
      return
    }

    if (prompt.includes('fixture:subagents-resumed')) {
      // Mirrors Claude Code 2.1.292 and later: task frames carry run and parent task IDs, and a
      // subagent's messages carry its agent ID, which stays the same when it is resumed.
      const agentCall = (id, description, parent, agent) => out({
        type: 'assistant',
        parent_tool_use_id: parent,
        ...agent ? { agent_id: agent } : {},
        message: {
          id: `spawn-${id}`,
          content: [{
            type: 'tool_use',
            id,
            name: 'Agent',
            input: { description, subagent_type: 'general-purpose', prompt: description },
          }],
        },
      })
      const said = (parent, agent, id, text) => out({
        type: 'assistant',
        parent_tool_use_id: parent,
        agent_id: agent,
        message: { id, content: [{ type: 'text', text }] },
      })
      const result = (parent, agent, toolId, text, outcome) => out({
        type: 'user',
        parent_tool_use_id: parent,
        ...agent ? { agent_id: agent } : {},
        message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: toolId, content: [{ type: 'text', text }] }] },
        ...outcome ? { tool_use_result: outcome } : {},
      })

      agentCall('agent-review', 'Review the change', null)
      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-review',
        run_id: 'run-1',
        tool_use_id: 'agent-review',
        description: 'Review the change',
        subagent_type: 'general-purpose',
        is_backgrounded: false,
        spawn_depth: 1,
        task_type: 'local_agent',
      })
      agentCall('agent-nested', 'Check the tests', 'agent-review', 'task-review')
      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-nested',
        run_id: 'run-n1',
        tool_use_id: 'agent-nested',
        parent_task_id: 'task-review',
        description: 'Check the tests',
        subagent_type: 'general-purpose',
        is_backgrounded: false,
        spawn_depth: 2,
        task_type: 'local_agent',
      })
      said('agent-nested', 'task-nested', 'nested-1', 'Tests pass.')
      out({
        type: 'system',
        subtype: 'task_notification',
        task_id: 'task-nested',
        run_id: 'run-n1',
        tool_use_id: 'agent-nested',
        status: 'completed',
        summary: 'Tests pass.',
        output_file: '',
      })
      result('agent-review', 'task-review', 'agent-nested', 'Tests pass.', { status: 'completed', agentId: 'task-nested', content: [{ type: 'text', text: 'Tests pass.' }] })
      said('agent-review', 'task-review', 'review-1', 'Looks good.')
      out({
        type: 'system',
        subtype: 'task_notification',
        task_id: 'task-review',
        run_id: 'run-1',
        tool_use_id: 'agent-review',
        status: 'completed',
        summary: 'Looks good.',
        output_file: '',
      })
      result(null, null, 'agent-review', 'Looks good.', { status: 'completed', agentId: 'task-review', content: [{ type: 'text', text: 'Looks good.' }] })
      // Frames of the first run may still arrive after it reported back.
      out({
        type: 'system',
        subtype: 'task_progress',
        task_id: 'task-review',
        run_id: 'run-1',
        tool_use_id: 'agent-review',
        description: 'Late progress of the first run',
        usage: { total_tokens: 1, tool_uses: 0, duration_ms: 1 },
      })
      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-review',
        run_id: 'run-1',
        tool_use_id: 'agent-review',
        description: 'Review the change',
        task_type: 'local_agent',
      })

      // The agent resumes the reviewer through another call. Its new run names that call, and
      // its messages only its agent ID.
      out({
        type: 'assistant',
        parent_tool_use_id: null,
        message: {
          id: 'send',
          content: [{
            type: 'tool_use',
            id: 'send-review',
            name: 'SendMessage',
            input: { to: 'task-review', message: 'Check the docs too.' },
          }],
        },
      })
      out({
        type: 'system',
        subtype: 'task_started',
        task_id: 'task-review',
        run_id: 'run-2',
        tool_use_id: 'send-review',
        description: 'Review the change',
        is_backgrounded: true,
        task_type: 'local_agent',
      })
      out({
        type: 'system',
        subtype: 'task_progress',
        task_id: 'task-review',
        run_id: 'run-1',
        tool_use_id: 'agent-review',
        description: 'Late progress of the first run',
        usage: { total_tokens: 1, tool_uses: 0, duration_ms: 1 },
      })
      said(null, 'task-review', 'review-2', 'Docs are fine too.')
      // Without an agent ID (before 2.1.292), its messages may name the call that resumed it.
      said('send-review', undefined, 'review-3', 'Changelog too.')
      out({
        type: 'system',
        subtype: 'task_notification',
        task_id: 'task-review',
        run_id: 'run-2',
        tool_use_id: 'send-review',
        status: 'completed',
        summary: 'Docs are fine too.',
        output_file: '',
        usage: { total_tokens: 700, tool_uses: 2, duration_ms: 400 },
      })
      result(null, null, 'send-review', 'Message delivered.')
      complete('Review resumed and finished', false, correlation)
      return
    }

    if (prompt.includes('fixture:subagents')) {
      // Mirrors Claude Code 2.1.280: one subagent in the foreground, one in the background.
      // Both stream their own messages, tagged with the tool use that started them.
      const agent = (id, description, background) => out({
        type: 'assistant',
        parent_tool_use_id: null,
        message: {
          id: `spawn-${id}`,
          content: [{
            type: 'tool_use',
            id,
            name: 'Agent',
            input: {
              description,
              subagent_type: 'general-purpose',
              prompt: `${description} in the workspace.`,
              run_in_background: background,
            },
          }],
        },
      })
      const task = (subtype, id, fields) => out({
        type: 'system',
        subtype,
        task_id: `task-${id}`,
        tool_use_id: id,
        ...fields,
      })
      const child = (parent, message) => out({ type: 'assistant', parent_tool_use_id: parent, message })
      const childResult = (parent, toolId, content) => out({
        type: 'user',
        parent_tool_use_id: parent,
        message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: toolId, content }] },
      })

      agent('agent-count', 'Count lines', false)
      task('task_started', 'agent-count', {
        description: 'Count lines',
        subagent_type: 'general-purpose',
        is_backgrounded: false,
        task_type: 'local_agent',
      })
      out({ type: 'user', parent_tool_use_id: 'agent-count', message: { role: 'user', content: [{ type: 'text', text: 'Count lines in the workspace.' }] } })
      agent('agent-list', 'List files', true)
      out({ type: 'system', subtype: 'background_tasks_changed', tasks: [{ task_id: 'task-agent-list', task_type: 'local_agent', description: 'List files' }] })
      task('task_started', 'agent-list', {
        description: 'List files',
        subagent_type: 'general-purpose',
        is_backgrounded: true,
        task_type: 'local_agent',
      })
      out({
        type: 'user',
        parent_tool_use_id: null,
        message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'agent-list', content: [{ type: 'text', text: 'Async agent launched successfully.' }] }] },
        tool_use_result: {
          isAsync: true,
          status: 'async_launched',
          agentId: 'task-agent-list',
          description: 'List files',
          prompt: 'List files in the workspace.',
        },
      })
      task('task_progress', 'agent-count', { description: 'Running wc -l', usage: { total_tokens: 1200, tool_uses: 1, duration_ms: 900 }, last_tool_name: 'Bash' })
      child('agent-count', {
        id: 'count-1',
        content: [{
          type: 'tool_use',
          id: 'count-bash',
          name: 'Bash',
          input: { command: 'wc -l *.txt' },
        }],
      })
      childResult('agent-count', 'count-bash', '3 total')
      child('agent-list', {
        id: 'list-1',
        content: [{
          type: 'tool_use',
          id: 'list-bash',
          name: 'Bash',
          input: { command: 'ls' },
        }],
      })
      childResult('agent-list', 'list-bash', 'one.txt\ntwo.txt')
      child('agent-count', { id: 'count-2', content: [{ type: 'text', text: 'Total: 3 lines.' }] })
      task('task_notification', 'agent-count', { status: prompt.includes('fixture:subagents-stopped') ? 'stopped' : 'completed', summary: 'Total: 3 lines.', usage: { total_tokens: 1500, tool_uses: 1, duration_ms: 1100 } })
      out({
        type: 'user',
        parent_tool_use_id: null,
        message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'agent-count', content: [{ type: 'text', text: '[Subagent hand-back] Total: 3 lines.' }] }] },
        tool_use_result: {
          status: 'completed',
          agentId: 'task-agent-count',
          content: [{ type: 'text', text: 'Total: 3 lines.' }],
          totalDurationMs: 1150,
          totalTokens: 1510,
          totalToolUseCount: 1,
        },
      })
      complete('Counted while the listing continues', false, correlation)
      setTimeout(() => {
        child('agent-list', { id: 'list-2', content: [{ type: 'text', text: 'Files: one.txt, two.txt' }] })
        out({ type: 'system', subtype: 'background_tasks_changed', tasks: [] })
        task('task_notification', 'agent-list', { status: 'completed', summary: 'Files: one.txt, two.txt', usage: { total_tokens: 900, tool_uses: 1, duration_ms: 700 } })
        setTimeout(() => {
          out({
            type: 'user',
            uuid: 'notification',
            origin: { kind: 'task-notification' },
            message: { role: 'user', content: [] },
          })
          complete('Both subagents reported back', false, { origin: { kind: 'task-notification' } })
        }, 100)
      }, 300)
      return
    }

    if (prompt.includes('fixture:batch')) {
      if (!question) {
        question = value.uuid
        return
      }

      complete('Both prompts processed', false, { user_message_uuids: [question, value.uuid], user_message_uuid: value.uuid })
      return
    }

    if (prompt.includes('fixture:hang'))
      return
    if (prompt.includes('fixture:fail')) {
      out({
        type: 'result',
        subtype: 'error_during_execution',
        is_error: true,
        result: 'Fixture usage limit reached',
      })
      return
    }

    if (prompt.includes('fixture:question')) {
      question = true
      out({ type: 'control_request', request_id: 'question-1', request: { subtype: 'can_use_tool', tool_name: 'AskUserQuestion', input: { questions: [{ question: 'Which approach?', options: [{ label: 'Small change', description: 'Keep scope focused' }, { label: 'Larger change', description: 'Expand scope' }] }] } } })
      return
    }

    out({
      type: 'assistant',
      message: {
        id: 'tool-message',
        content: [{
          type: 'tool_use',
          id: `tool-${process.pid}`,
          name: 'Bash',
          input: { command: 'printf fixture' },
        }],
      },
    })
    out({ type: 'user', message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: `tool-${process.pid}`, content: 'fixture' }] } })
    if (prompt.includes('fixture:slow'))
      setTimeout(complete, 1500, undefined, false, correlation)
    else
      complete(undefined, prompt.includes('fixture:thinking'), correlation)
  })
}
