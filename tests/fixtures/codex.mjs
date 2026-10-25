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
      const startupFailure = path.join(process.env.CODEX_HOME, 'fixture-startup-failures.json')
      if (existsSync(startupFailure)) {
        const failure = JSON.parse(readFileSync(startupFailure, 'utf8'))
        if (request.method === failure.method && failure.remaining > 0) {
          failure.remaining--
          failure.pids = [...(failure.pids ?? []), process.pid]
          writeFileSync(startupFailure, JSON.stringify(failure))
          process.exit(0)
        }
      }

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

      if (request.method === 'account/rateLimits/read' && process.env.CAIRN_FIXTURE_USAGE)
        result = JSON.parse(readFileSync(process.env.CAIRN_FIXTURE_USAGE, 'utf8'))[id] ?? result
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
    throw new Error(`Unsupported Codex fixture command: ${args[0]}`)
  }
}

void main()
