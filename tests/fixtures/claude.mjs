#!/usr/bin/env node
// Synthetic official-CLI protocol fixture. Never connects to a model provider.
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
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
  appendFileSync(log, `${JSON.stringify({ args })}\n`)
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

    if (prompt.includes('fixture:background')) {
      const ambient = prompt.includes('fixture:background-ambient')
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
        }, 500)
      }

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
