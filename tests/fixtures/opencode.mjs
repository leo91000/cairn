#!/usr/bin/env node
import { Buffer } from 'node:buffer'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import http from 'node:http'
import path from 'node:path'
import process from 'node:process'

if (process.argv.includes('--version')) {
  process.stdout.write('1.19.0\n')
  process.exit(0)
}

async function main() {
  const root = path.join(process.env.XDG_DATA_HOME, 'opencode')
  await mkdir(root, { recursive: true })
  const file = path.join(root, 'fixture-sessions.json')
  const sessions = JSON.parse(await readFile(file, 'utf8').catch(() => '{}'))
  const streams = new Set()
  const questions = new Map()
  const hanging = new Set()
  const auth = JSON.parse(process.env.OPENCODE_AUTH_CONTENT || '{}')
  const headers = `Basic ${Buffer.from(`cairn:${process.env.OPENCODE_SERVER_PASSWORD}`).toString('base64')}`
  let counter = 100
  const id = prefix => `${prefix}_${(Date.now() * 4096 + counter++).toString(16)}${randomUUID().replaceAll('-', '').slice(0, 14)}`
  const persist = () => writeFile(file, JSON.stringify(sessions))

  function emit(type, properties) {
    for (const stream of streams)
      stream.write(`data: ${JSON.stringify({ type, properties })}\n\n`)
  }

  function nativeModel(model, name, variants) {
    return {
      id: model,
      name,
      status: 'active',
      variants: Object.fromEntries(variants.map(value => [value, {}])),
    }
  }

  const providers = {
    all: [
      {
        id: 'opencode',
        name: 'OpenCode',
        models: {
          'ling-3.1-flash-free': nativeModel('ling-3.1-flash-free', 'Ling 3.1 Flash Free', ['low', 'medium', 'high']),
          'cat-jam-free': nativeModel('cat-jam-free', 'Cat Jam Free', ['low', 'medium', 'high', 'xhigh', 'max']),
        },
      },
      {
        id: 'opencode-go',
        name: 'OpenCode Go',
        models: {
          'ling-3.1-flash-free': nativeModel('ling-3.1-flash-free', 'Ling 3.1 Flash Free', ['medium', 'high']),
        },
      },
      {
        id: 'openrouter',
        name: 'OpenRouter',
        models: {
          'anthropic/claude-sonnet-4.6': nativeModel('anthropic/claude-sonnet-4.6', 'Claude Sonnet 4.6', ['low', 'high']),
        },
      },
      { id: 'openai', name: 'OpenAI', models: { 'fixture-model': nativeModel('fixture-model', 'Fixture model', []) } },
    ],
    default: { opencode: 'ling-3.1-flash-free', openrouter: 'anthropic/claude-sonnet-4.6', openai: 'fixture-model' },
    connected: Object.keys(auth),
  }

  async function finish(session, parent, text, finish = 'stop') {
    const now = Date.now()
    const message = {
      info: {
        id: id('msg'),
        sessionID: session.id,
        parentID: parent,
        role: 'assistant',
        finish,
        time: { created: now, completed: now + 1 },
      },
      parts: [],
    }
    const part = {
      id: id('prt'),
      sessionID: session.id,
      messageID: message.info.id,
      type: 'text',
      text: '',
      time: { start: now },
    }
    emit('message.part.updated', { part })
    emit('message.part.delta', {
      sessionID: session.id,
      messageID: message.info.id,
      partID: part.id,
      field: 'text',
      delta: text,
    })
    part.text = text
    part.time.end = now + 1
    message.parts.push(part)
    session.messages.push(message)
    await persist()
    emit('message.updated', { info: message.info })
    emit('message.part.updated', { part })
    emit('session.status', { sessionID: session.id, status: { type: 'idle' } })
  }

  async function prompt(session, input) {
    const provider = input.model.providerID
    if (!auth[provider]?.key)
      throw new Error('The selected provider did not receive the managed key')
    const body = input.parts.filter(part => part.type === 'text').map(part => part.text).join('\n')
    const user = {
      info: {
        id: input.messageID,
        sessionID: session.id,
        role: 'user',
        time: { created: Date.now() },
      },
      parts: input.parts.map(part => ({
        ...part,
        id: id('prt'),
        sessionID: session.id,
        messageID: input.messageID,
      })),
    }
    session.messages.push(user)
    session.receivedManagedKey = true
    session.lastModel = input.model
    session.lastVariant = input.variant || ''
    await persist()
    emit('message.updated', { info: user.info })
    for (const part of user.parts)
      emit('message.part.updated', { part })
    emit('session.status', { sessionID: session.id, status: { type: 'busy' } })
    // A subscription past its limit; Auto continues on the next connection.
    if (body.includes('fixture:opencode-limit') && provider === 'opencode-go') {
      emit('session.error', { sessionID: session.id, error: { name: 'APIError', data: { statusCode: 429, message: 'Go limit reached' } } })
      return
    }

    if (body.includes('fixture:opencode-error')) {
      emit('session.error', { sessionID: session.id, error: { name: 'APIError', data: { statusCode: 401, message: 'Sensitive provider detail' } } })
      return
    }

    if (body.includes('fixture:opencode-continuation')) {
      const continuation = {
        info: {
          id: id('msg'),
          sessionID: session.id,
          role: 'user',
          time: { created: Date.now() },
        },
        parts: [],
      }
      continuation.parts.push({
        id: id('prt'),
        sessionID: session.id,
        messageID: continuation.info.id,
        type: 'text',
        text: 'Continue if you have next steps...',
      })
      session.messages.push(continuation)
      await persist()
      emit('message.updated', { info: continuation.info })
      emit('message.part.updated', { part: continuation.parts[0] })
      await finish(session, continuation.info.id, 'Synthetic continuation response')
      return
    }

    if (body.includes('fixture:opencode-hang')) {
      hanging.add(session.id)
      return
    }

    if (body.includes('fixture:opencode-question')) {
      const request = id('que')
      questions.set(request, { session, parent: input.messageID })
      emit('question.asked', { id: request, sessionID: session.id, questions: [{ question: 'Which layout should I build?', header: 'Layout', options: [{ label: 'Compact', description: 'A focused workspace' }, { label: 'Spacious', description: 'More room for the details' }] }] })
      return
    }

    const tool = {
      id: id('prt'),
      sessionID: session.id,
      messageID: id('msg'),
      type: 'tool',
      tool: 'bash',
      state: { status: 'running', input: { command: 'printf "Workspace ready\\n"' } },
    }
    emit('message.part.updated', { part: tool })
    tool.state = { ...tool.state, status: 'completed', output: 'Workspace ready\n' }
    emit('message.part.updated', { part: tool })
    hanging.delete(session.id)
    await finish(session, input.messageID, 'I checked the workspace and prepared a compact status panel. The model and API key were selected through OpenCode.', body.includes('fixture:opencode-length') ? 'length' : 'stop')
  }

  const server = http.createServer(async (request, response) => {
    const json = (status, value) => {
      response.writeHead(status, { 'content-type': 'application/json' })
      response.end(JSON.stringify(value))
    }

    if (request.headers.authorization !== headers) {
      json(401, { error: 'Unauthorized' })
      return
    }

    const url = new URL(request.url, 'http://localhost')
    if (request.method === 'GET' && url.pathname === '/global/health') {
      json(200, { healthy: true, version: '1.19.0' })
      return
    }

    if (request.method === 'GET' && url.pathname === '/provider') {
      json(200, providers)
      return
    }

    if (request.method === 'GET' && url.pathname === '/event') {
      response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
      response.write('data: {"type":"server.connected","properties":{}}\n\n')
      streams.add(response)
      request.on('close', () => streams.delete(response))
      return
    }

    let input = ''
    for await (const chunk of request)
      input += chunk
    const body = input ? JSON.parse(input) : {}
    if (request.method === 'POST' && url.pathname === '/session') {
      const session = { id: id('ses'), directory: url.searchParams.get('directory'), messages: [] }
      sessions[session.id] = session
      await persist()
      json(200, { id: session.id, directory: session.directory })
      return
    }

    const question = url.pathname.match(/^\/question\/(que_\w+)\/reply$/)
    if (question && request.method === 'POST') {
      const pending = questions.get(question[1])
      if (!pending) {
        json(404, {})
        return
      }

      questions.delete(question[1])
      json(200, true)
      emit('question.replied', { sessionID: pending.session.id, requestID: question[1] })
      await finish(pending.session, pending.parent, `I will use the ${body.answers[0].join(', ')} layout.`)
      return
    }

    const route = url.pathname.match(/^\/session\/(ses_\w+)(\/message|\/prompt_async|\/abort)?$/)
    const session = route && sessions[route[1]]
    if (!session) {
      json(404, {})
      return
    }

    if (request.method === 'GET') {
      json(200, route[2] === '/message' ? session.messages : { id: session.id, directory: session.directory })
      return
    }

    if (route[2] === '/prompt_async') {
      response.writeHead(204)
      response.end()
      setTimeout(() => void prompt(session, body).catch(() => emit('session.error', { sessionID: session.id, error: { name: 'FixtureError' } })), 20)
      return
    }

    json(200, true)
  })
  server.listen(0, '127.0.0.1', () => {
    process.stdout.write(`opencode server listening on http://127.0.0.1:${server.address().port}\n`)
  })
}

main().catch(() => process.exit(1))
