#!/usr/bin/env node
import { Buffer } from 'node:buffer'
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'
import activityEvents from './activity-events.json' with { type: 'json' }
import { chatFixture } from './chat-rpc.mjs'

async function main() {
  const args = process.argv.slice(2)
  if (args.includes('--version')) {
    process.stdout.write('fixture-codex 1.0\n')
    process.exit(0)
  }

  if (args.includes('app-server')) {
    const launchRecord = path.join(process.env.CODEX_HOME, 'fixture-launch-args.json')
    if (existsSync(launchRecord))
      writeFileSync(launchRecord, JSON.stringify(args))

    const chat = chatFixture()
    let loginTimer
    const lines = createInterface({ input: process.stdin })
    for await (const line of lines) {
      const request = JSON.parse(line)
      const lifecycle = path.join(process.env.CODEX_HOME, 'fixture-lifecycle.jsonl')
      if (existsSync(lifecycle))
        appendFileSync(lifecycle, `${JSON.stringify({ pid: process.pid, method: request.method })}\n`)
      if (request.id === undefined)
        continue
      const initializationFailure = path.join(process.env.CODEX_HOME, 'fixture-initialize-error')
      const initializationWait = path.join(process.env.CODEX_HOME, 'fixture-initialize-wait')
      if (request.method === 'initialize' && existsSync(initializationWait)) {
        process.on('SIGTERM', () => {})
        setInterval(() => {}, 1000)
        writeFileSync(initializationWait, `${process.pid}`)
        continue
      }

      if (request.method === 'initialize' && existsSync(initializationFailure)) {
        process.on('SIGTERM', () => {})
        setInterval(() => {}, 1000)
        writeFileSync(initializationFailure, String(process.pid))
        process.stdout.write(`${JSON.stringify({ id: request.id, error: { code: -32000, message: 'Initialization failed' } })}\n`)
        continue
      }

      if (chat(request))
        continue
      let result = {}
      const authPath = path.join(process.env.CODEX_HOME, 'auth.json')
      const auth = existsSync(authPath) ? JSON.parse(readFileSync(authPath, 'utf8')) : null
      const id = auth?.tokens.account_id ?? 'fixture'
      if (request.method === 'account/login/cancel') {
        clearTimeout(loginTimer)
        clearInterval(loginTimer)
        writeFileSync(path.join(process.env.CODEX_HOME, 'fixture-login-cancelled'), request.params.loginId)
        result = { status: 'canceled' }
      }

      if (request.method === 'account/login/start' && request.params.type === 'chatgptAuthTokens') {
        if (!request.params.accessToken || !request.params.chatgptAccountId || existsSync(authPath))
          throw new Error('External auth must not have refresh credentials')
        globalThis.fixtureAccountId = request.params.chatgptAccountId
        process.stdout.write(`${JSON.stringify({ id: request.id, result: { type: 'chatgptAuthTokens' } })}\n`)
        continue
      }

      if (request.method === 'account/read' && request.params?.refreshToken && auth) {
        auth.tokens.access_token += '-refreshed'
        auth.tokens.refresh_token += '-rotated'
        writeFileSync(authPath, JSON.stringify(auth))
      }

      if (request.method === 'account/login/start') {
        const controlPath = path.join(process.env.CODEX_HOME, 'fixture-login.json')
        const control = existsSync(controlPath) ? JSON.parse(readFileSync(controlPath, 'utf8')) : {}
        if (request.params.type !== 'chatgptDeviceCode')
          throw new Error('Expected structured device sign-in')
        if (control.mode === 'unsupported') {
          process.stdout.write(`${JSON.stringify({ id: request.id, error: { code: -32601, message: 'synthetic secret must not leak' } })}\n`)
          continue
        }

        const loginId = 'fixture-device-login'
        const complete = () => {
          mkdirSync(process.env.CODEX_HOME, { recursive: true })
          if (control.mode !== 'failure') {
            const id = control.identity ?? path.basename(path.dirname(process.env.CODEX_HOME))
            const idToken = `header.${Buffer.from(JSON.stringify({ sub: id })).toString('base64url')}.signature`
            writeFileSync(authPath, JSON.stringify({
              tokens: {
                access_token: 'synthetic-access',
                refresh_token: 'synthetic-refresh',
                account_id: id,
                id_token: idToken,
              },
            }), { mode: 0o600 })
          }

          process.stdout.write(`${JSON.stringify({ method: 'account/login/completed', params: { loginId, success: control.mode !== 'failure', error: 'synthetic secret must not leak' } })}\n`)
        }

        result = {
          type: 'chatgptDeviceCode',
          loginId,
          verificationUrl: control.url ?? 'https://auth.openai.com/codex/device',
          userCode: 'ABCD-12345',
        }
        // An unrelated login notification must never finish this attempt.
        process.stdout.write(`${JSON.stringify({ method: 'account/login/completed', params: { loginId: 'unrelated-login', success: true } })}\n`)
        process.stderr.write('\x1B[94mIGNORED-CODE\x1B[0m\n')
        if (control.mode === 'immediate') {
          complete()
        }
        else if (control.mode === 'hold') {
          loginTimer = setInterval(() => {
            if (existsSync(path.join(process.env.CODEX_HOME, 'fixture-login-approve'))) {
              clearInterval(loginTimer)
              complete()
            }
          }, 30)
        }
        else if (control.mode === 'disconnect') {
          loginTimer = setTimeout(() => process.exit(1), 120)
        }
        else {
          loginTimer = setTimeout(complete, 120)
        }
      }

      if (request.method === 'model/list') {
        const fast = {
          id: 'fast-id',
          model: 'fixture-fast',
          displayName: 'Quick coder',
          description: 'Fast everyday coding',
          hidden: false,
          isDefault: true,
          defaultReasoningEffort: 'low',
          supportedReasoningEfforts: [{ reasoningEffort: 'low', description: 'Quick responses' }, { reasoningEffort: 'high', description: 'Think through complex changes' }],
        }
        const deep = {
          id: 'deep-id',
          model: 'fixture-deep',
          displayName: 'Deep thinker',
          description: 'Complex investigations',
          hidden: false,
          isDefault: false,
          defaultReasoningEffort: 'medium',
          supportedReasoningEfforts: [{ reasoningEffort: 'medium', description: 'Balanced depth' }, { reasoningEffort: 'ultra', description: 'Take time for the hardest problems' }],
        }
        result = request.params.cursor
          ? {
              data: [{
                ...fast,
                model: 'fixture-hidden',
                hidden: true,
                isDefault: false,
              }],
              nextCursor: null,
            }
          : { data: id === 'fast-only' ? [fast] : [fast, deep], nextCursor: 'page-2' }
      }

      const conversation = path.join(process.env.CODEX_HOME, 'fixture-conversation.json')
      if (request.method === 'thread/read' || request.method === 'thread/list') {
        if (!existsSync(conversation)) {
          process.stdout.write(`${JSON.stringify({ id: request.id, error: { code: -32000, message: 'Session missing' } })}\n`)
          continue
        }

        const thread = JSON.parse(readFileSync(conversation, 'utf8'))
        result = request.method === 'thread/read' ? { thread } : { data: [thread] }
      }

      if (request.method === 'account/read')
        result = { account: auth ? { type: 'chatgpt', email: `${id}@example.test`, planType: 'plus' } : null }
      if (request.method === 'account/rateLimits/read') {
        result = {
          ordinaryUsageAllowed: true,
          accountId: id,
          rateLimits: {
            limitId: 'codex',
            limitName: 'Codex',
            primary: { usedPercent: 25, windowDurationMins: 300, resetsAt: Math.floor(Date.now() / 1000) + 7200 },
            secondary: { usedPercent: 40, windowDurationMins: 10080, resetsAt: Math.floor(Date.now() / 1000) + 172800 },
          },
        }
      }

      if (request.method === 'account/rateLimits/read' && process.env.LEO_FIXTURE_USAGE)
        result = JSON.parse(readFileSync(process.env.LEO_FIXTURE_USAGE, 'utf8'))[id] ?? result
      process.stdout.write(`${JSON.stringify({ id: request.id, result })}\n`)
    }

    return
  }

  if (args[0] === 'login' && args[1] === 'status') {
    process.stderr.write('Logged in using ChatGPT\n')
    process.exit(0)
  }

  if (args[0] === 'login') {
    process.stdout.write(
      'Open https://auth.openai.com/codex/device and enter ABCD-12345\n',
    )
    setTimeout(() => {
      mkdirSync(process.env.CODEX_HOME, { recursive: true })
      const id = path.basename(path.dirname(process.env.CODEX_HOME))
      writeFileSync(path.join(process.env.CODEX_HOME, 'auth.json'), JSON.stringify({ tokens: { access_token: 'synthetic-access', refresh_token: 'synthetic-refresh', account_id: id } }), { mode: 0o600 })
      process.exit(0)
    }, 120)
  }
  else {
    let prompt = ''
    for await (const chunk of process.stdin)
      prompt += chunk
    const emit = value => process.stdout.write(`${JSON.stringify(value)}\n`)
    mkdirSync(process.env.CODEX_HOME, { recursive: true })
    const conversation = path.join(process.env.CODEX_HOME, 'fixture-conversation.json')
    if (!args.includes('resume'))
      writeFileSync(conversation, JSON.stringify({ id: 'fixture-session', cwd: process.cwd(), parentThreadId: null }))
    emit({ type: 'thread.started', thread_id: 'fixture-session' })
    const restartMarker = path.join(process.env.CODEX_HOME, 'fixture-restart.json')
    if (prompt.includes('fixture:restart') && !args.includes('resume')) {
      writeFileSync(restartMarker, JSON.stringify({ cwd: process.cwd() }))
      writeFileSync(path.join(process.cwd(), 'restart-work.txt'), 'preserved before restart')
      setInterval(emit, 100, { type: 'progress', item: { text: 'Waiting for restart' } })
      return
    }

    if (args.includes('resume') && existsSync(restartMarker)) {
      const previous = JSON.parse(readFileSync(restartMarker, 'utf8'))
      if (previous.cwd !== process.cwd() || readFileSync(path.join(process.cwd(), 'restart-work.txt'), 'utf8') !== 'preserved before restart')
        throw new Error('Restart lost workspace')
      writeFileSync(args[args.indexOf('--output-last-message') + 1], '# Resumed\nThe saved conversation and work survived the restart.')
      emit({ type: 'turn.completed', usage: { input_tokens: 1, output_tokens: 1 } })
      return
    }

    const marker = path.join(process.env.CODEX_HOME, 'fixture-resume.json')
    if (prompt.includes('fixture:exhaust')) {
      mkdirSync(process.env.CODEX_HOME, { recursive: true })
      const auth = JSON.parse(readFileSync(path.join(process.env.CODEX_HOME, 'auth.json'), 'utf8'))
      writeFileSync(marker, JSON.stringify({ account: auth.tokens.account_id, cwd: process.cwd(), allowSameAccount: prompt.includes('fixture:banked-reset') }))
      writeFileSync(path.join(process.cwd(), 'preserved-work.txt'), 'work before exhaustion')
      emit({ type: 'turn.failed', error: { message: 'You\'ve hit your usage limit. Try again later.' } })
      process.exitCode = 1
      return
    }

    if (args.includes('resume')) {
      const previous = JSON.parse(readFileSync(marker, 'utf8'))
      const auth = JSON.parse(readFileSync(path.join(process.env.CODEX_HOME, 'auth.json'), 'utf8'))
      if ((previous.account === auth.tokens.account_id && !previous.allowSameAccount) || previous.cwd !== process.cwd() || args[args.indexOf('resume') + 1] !== 'fixture-session' || readFileSync(path.join(process.cwd(), 'preserved-work.txt'), 'utf8') !== 'work before exhaustion')
        throw new Error('Resume did not preserve context or switch accounts')
      emit({ type: 'item.completed', item: { text: 'Resumed original session on another account with workspace intact' } })
    }

    process.stdout.write('non-JSON diagnostic\n')
    emit({ type: 'item.completed', item: { text: 'Checking the project' } })
    if (prompt.includes('fixture:mcp')) {
      const { Client, StreamableHTTPClientTransport } = await import('@modelcontextprotocol/client')
      const configuration = args.find(arg => arg.startsWith('mcp_servers.') && arg.includes('"url"='))
      const url = JSON.parse(configuration.match(/"url"=("[^"]+")/)[1])
      const client = new Client({ name: 'fixture-worker', version: '1' })
      try {
        await client.connect(new StreamableHTTPClientTransport(new URL(url), { requestInit: { headers: { Authorization: `Bearer ${process.env.LEO_MCP_RUN_TOKEN}` } } }))
        const result = await client.callTool({ name: 'echo', arguments: { message: `MCP subprocess passed: ${process.env.LEO_MCP_RUN_TOKEN}` } })
        emit({ type: 'item.completed', item: { text: result.content[0].text } })
        writeFileSync(args[args.indexOf('--output-last-message') + 1], result.content[0].text)
      }
      finally {
        await client.close()
      }

      return
    }

    if (prompt.includes('fixture:activity')) {
      for (const event of activityEvents)
        emit(event)
    }

    if (prompt.includes('fixture:hang')) {
      setInterval(emit, 100, {
        type: 'progress',
        item: { text: 'Still working' },
      })
    }
    else {
      await new Promise(resolve => setTimeout(resolve, 100))
      process.stderr.write('Bearer test-token-value\n')
      if (prompt.includes('fixture:fail'))
        process.exit(2)
      emit({
        type: 'turn.completed',
        usage: { input_tokens: 12, output_tokens: 8 },
      })
      const output = args[args.indexOf('--output-last-message') + 1]
      writeFileSync(output, '# Complete\nThe fixture task passed.')
    }
  }
}

void main()
