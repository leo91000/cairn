#!/usr/bin/env node
// A stand-in for Google's Antigravity CLI (`agy`) 1.5.14: the commands and the
// stream-json protocol Cairn's adapter relies on, with scripted failures.
//
// Scenarios follow markers in a user message:
//   fixture:agy-tools         list_dir, run_command and write_file calls, each through the hooks
//   fixture:agy-slow=<ms>     the response waits <ms> after its user step
//   fixture:agy-429           a quota failure (AGY_ERROR with RESOURCE_EXHAUSTED)
//   fixture:agy-command       a long command in its own session, its pid in $HOME/agy-command.pid
//   fixture:agy-expired       a Google access token that expires during the turn (once per token)
// Environment:
//   CAIRN_AGY_FIXTURE_LOG     JSON-lines record of what each process received (default $HOME/agy-fixture.jsonl,
//                             in the run's native state also for a plan turn's fresh HOME)
//   CAIRN_AGY_FIXTURE_EMAIL   the Google account a sign-in completes for
//   CAIRN_AGY_FIXTURE_VERSION the release `--version` prints
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  appendFileSync,
  chmodSync,
  existsSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import path from 'node:path'
import process from 'node:process'
import { createInterface } from 'node:readline'

const args = process.argv.slice(2)
const home = process.env.HOME
const state = path.join(home, '.gemini/antigravity-cli')
const tokenFile = path.join(state, 'antigravity-oauth-token')
const logFile = process.env.CAIRN_AGY_FIXTURE_LOG
  || path.join(path.basename(home) === 'plan-turn' ? path.dirname(home) : home, 'agy-fixture.jsonl')

function log(record) {
  try {
    appendFileSync(logFile, `${JSON.stringify({ pid: process.pid, ...record })}\n`)
  }
  catch {}
}

function readJson(file) {
  try {
    return JSON.parse(readFileSync(file, 'utf8'))
  }
  catch {
    return null
  }
}

function writePrivate(file, value) {
  mkdirSync(path.dirname(file), { recursive: true, mode: 0o700 })
  const temporary = `${file}.${process.pid}.tmp`
  writeFileSync(temporary, JSON.stringify(value), { mode: 0o600 })
  chmodSync(temporary, 0o600)
  renameSync(temporary, file)
}

const hourFromNow = () => new Date(Date.now() + 3600_000).toISOString()

/** The credential this process runs with, as `agy` resolves it. */
// A Gemini API key counts only with Gemini as the model provider, from `--settings` or the
// settings file, as on 1.5.14.
function credential(settings = readJson(path.join(state, 'settings.json')) ?? {}) {
  const token = readJson(tokenFile)
  if (token?.token?.access_token)
    return { kind: 'google', token }
  if (process.env.AGY_ADC_AUTH === 'true' && process.env.GOOGLE_APPLICATION_CREDENTIALS && existsSync(process.env.GOOGLE_APPLICATION_CREDENTIALS))
    return { kind: 'google-cloud' }
  if (process.env.GEMINI_API_KEY && settings.modelProvider === 'gemini')
    return { kind: 'gemini' }
  return null
}

const catalogs = {
  'gemini': [
    ['gemini-4-neon-low', 'Gemini 4 Neon (Low)'],
    ['gemini-4-neon-high', 'Gemini 4 Neon (High)'],
    ['gemini-4-neon-xhigh', 'Gemini 4 Neon (XHigh)'],
    ['gemini-3.5-flash-medium', 'Gemini 3.5 Flash (Medium)'],
    ['gemini-3.5-flash', 'Gemini 3.5 Flash (High)'],
    ['gemini-3-flash-preview', 'Gemini 3 Flash'],
  ],
  'google-cloud': [
    ['gemini-4-neon-low', 'Gemini 4 Neon (Low)'],
    ['gemini-4-neon-high', 'Gemini 4 Neon (High)'],
    ['claude-sonnet-4-6-thinking', 'Claude Sonnet 4.6 (Thinking)'],
  ],
  'google': [
    ['gemini-4-neon-high', 'Gemini 4 Neon (High)'],
    ['gemini-4-neon-xhigh', 'Gemini 4 Neon (XHigh)'],
    ['gemini-3.5-flash', 'Gemini 3.5 Flash (High)'],
  ],
}

function expired(token) {
  return !(Date.parse(token?.token?.expiry ?? '') > Date.now())
}

/** Renews a saved Google token that has a refresh token, as `agy` does before a request. */
function renewToken(token) {
  if (!expired(token))
    return true
  if (!token.token.refresh_token)
    return false
  token.token.access_token = `ya29.fixture-renewed-${randomUUID()}`
  token.token.expiry = hourFromNow()
  token.token.expires_in = 3599
  writePrivate(tokenFile, token)
  return true
}

function models() {
  process.stdout.write('Fetching available models...\n')
  const current = credential()
  log({ type: 'models', kind: current?.kind ?? null, token: current?.token ?? null })
  if (!current) {
    process.stdout.write('Error: Please sign in to view available models. Launch the CLI without arguments to sign in.\n')
    process.exit(1)
  }

  if (current.kind === 'google' && !renewToken(current.token)) {
    process.stderr.write('Error: oauth2: token expired and refresh token is not set\n')
    process.exit(1)
  }

  for (const [slug, name] of catalogs[current.kind])
    process.stdout.write(`${slug}\t${name}\n`)
  process.exit(0)
}

function login() {
  process.stdout.write('Open this link in a browser to log in with Google:\n\n  https://accounts.google.com/o/oauth2/auth?client_id=fixture.apps.googleusercontent.com&redirect_uri=https%3A%2F%2Fantigravity.google%2Fauth&response_type=code&scope=openid+email&state=fixture\n\nThen paste the authorization code, or the full URL of the page you land on, and press Enter: ')
  const lines = createInterface({ input: process.stdin })
  let answered = false
  lines.on('line', (line) => {
    if (answered)
      return
    answered = true
    const code = line.trim()
    log({ type: 'login', code })
    if (!code || code === 'invalid') {
      process.stderr.write('\nError: the authorization code was not accepted\n')
      process.exit(1)
    }

    const email = process.env.CAIRN_AGY_FIXTURE_EMAIL || 'fixture.user@gmail.com'
    writePrivate(tokenFile, {
      token: {
        access_token: `ya29.fixture-${randomUUID()}`,
        token_type: 'Bearer',
        refresh_token: '1//fixture-refresh-token',
        expiry: hourFromNow(),
        expires_in: 3599,
      },
      auth_method: 'consumer',
      id_token_email: email,
    })
    process.stdout.write(`\nLogged in as ${email}.\n`)
    process.exit(0)
  })
  lines.on('close', () => {
    if (answered)
      return
    process.stderr.write('\nlogin cancelled: no authorization code was entered\n')
    process.exit(1)
  })
}

function option(name) {
  const index = args.indexOf(name)
  return index < 0 ? undefined : args[index + 1]
}

/** Splits a POSIX command line of plain words and single-quoted words. */
function words(command) {
  const result = []
  for (const match of command.matchAll(/'((?:[^']|'\\'')*)'|(\S+)/g))
    result.push(match[1] !== undefined ? match[1].replaceAll('\'\\\'\'', '\'') : match[2])
  return result
}

function runHook(command, input, timeout) {
  return new Promise((resolve) => {
    const [executable, ...rest] = words(command)
    const child = spawn(executable, rest, { stdio: ['pipe', 'pipe', 'pipe'] })
    let stdout = ''
    const timer = setTimeout(() => child.kill('SIGKILL'), timeout * 1000)
    child.stdout.on('data', chunk => stdout += chunk)
    child.on('error', () => resolve(''))
    child.on('close', () => {
      clearTimeout(timer)
      resolve(stdout)
    })
    child.stdin.on('error', () => {})
    child.stdin.end(JSON.stringify(input))
  })
}

/** Runs every configured PreToolUse hook; only an allow decision from each one allows. */
async function hooksAllow(call, context) {
  const config = readJson(path.join(home, '.gemini/config/hooks.json')) ?? {}
  for (const group of Object.values(config)) {
    for (const entry of group?.PreToolUse ?? []) {
      if (!new RegExp(`^(?:${entry.matcher ?? '.*'})$`).test(call.name))
        continue
      for (const hook of entry.hooks ?? []) {
        const input = {
          hookEventName: 'PreToolUse',
          toolCall: call,
          conversationId: context.conversation,
          cwd: process.cwd(),
        }
        const output = await runHook(hook.command, input, hook.timeout ?? 10)
        let decision = null
        try {
          decision = JSON.parse(output.trim().split('\n').at(-1) ?? '')
        }
        catch {}

        log({
          type: 'hook',
          tool: call.name,
          decision: decision?.decision ?? null,
          reason: decision?.reason ?? null,
          output: decision ? undefined : output.slice(0, 400),
        })
        if (decision?.decision !== 'allow')
          return { allowed: false, reason: decision?.reason || 'Tool call blocked by a PreToolUse hook.' }
      }
    }
  }

  return { allowed: true }
}

/** The permission rules of `--settings`: `command(<prefix>)` and `<tool>(<prefix>)`, `*` for all. */
function deniedBySettings(settings, call) {
  const rules = settings?.permissions?.deny ?? []
  const [kind, subject] = call.name === 'run_command'
    ? ['command', call.args.CommandLine ?? '']
    : [call.name, call.args.Path ?? call.args.DirectoryPath ?? '']
  return rules.some((rule) => {
    const match = /^([\w-]+)\((.*)\)$/.exec(rule)
    return match && match[1] === kind && (match[2] === '*' || subject.startsWith(match[2]))
  })
}

async function print() {
  const settings = JSON.parse(option('--settings') ?? '{}')
  const model = option('--model')
  const mode = option('--mode') ?? 'default'
  const resumed = option('--conversation')
  const envNames = ['HOME', 'BASH_ENV', 'AGY_CLI_DISABLE_AUTO_UPDATE', 'CAIRN_ANTIGRAVITY_HOME', 'GEMINI_API_KEY', 'AGY_ADC_AUTH', 'GOOGLE_APPLICATION_CREDENTIALS', 'GOOGLE_CLOUD_PROJECT', 'GOOGLE_CLOUD_QUOTA_PROJECT', 'GOOGLE_CLOUD_LOCATION', 'DBUS_SESSION_BUS_ADDRESS', 'CAIRN_PREVIOUS_BASH_ENV']
  const env = Object.fromEntries(envNames.filter(name => process.env[name] !== undefined).map(name => [name, process.env[name]]))
  const adcPath = process.env.GOOGLE_APPLICATION_CREDENTIALS
  const adc = adcPath && existsSync(adcPath) ? { content: readJson(adcPath), mode: statSync(adcPath).mode & 0o777 } : null
  const token = existsSync(tokenFile) ? { content: readJson(tokenFile), mode: statSync(tokenFile).mode & 0o777 } : null
  log({
    type: 'launch',
    args,
    settings,
    model,
    mode,
    conversation: resumed ?? null,
    cwd: process.cwd(),
    env,
    adc,
    token,
    geminiMd: existsSync(path.join(home, '.gemini/GEMINI.md')) ? readFileSync(path.join(home, '.gemini/GEMINI.md'), 'utf8') : null,
    mcpConfig: readJson(path.join(home, '.gemini/config/mcp_config.json')),
  })
  const write = value => process.stdout.write(`${JSON.stringify(value)}\n`)
  const fail = (error) => {
    process.stderr.write(`AGY_ERROR: ${JSON.stringify(error)}\n`)
  }

  const current = credential({ ...readJson(path.join(state, 'settings.json')), ...settings })
  if (!current) {
    process.stderr.write('Error: authentication required. Run \'agy\' to log in, then retry.\nerror: authentication failed or timed out\n')
    write({
      event: 'result',
      result: {
        conversation_id: '',
        status: 'ERROR',
        response: '',
        error: 'authentication failed or timed out',
        num_turns: 0,
        finished_reason: 'error',
      },
    })
    process.exit(1)
  }

  if (!model || !catalogs[current.kind].some(([slug]) => slug === model)) {
    fail({ short_error: `invalid model selection: ${model}`, status: 'INVALID_ARGUMENT', error_code: 400 })
    process.exit(1)
  }

  process.removeAllListeners('warning')
  const { DatabaseSync } = await import('node:sqlite')
  const conversations = path.join(state, 'conversations')
  mkdirSync(conversations, { recursive: true, mode: 0o700 })
  const conversation = resumed ?? randomUUID()
  const file = path.join(conversations, `${conversation}.db`)
  if (resumed && !existsSync(file)) {
    fail({ short_error: `conversation ${resumed} not found`, status: 'NOT_FOUND', error_code: 404 })
    process.exit(1)
  }

  const db = new DatabaseSync(file)
  db.exec('CREATE TABLE IF NOT EXISTS steps (step_index INTEGER PRIMARY KEY, role TEXT NOT NULL, content TEXT NOT NULL)')
  db.exec('CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)')
  const insert = db.prepare('INSERT INTO steps (step_index, role, content) VALUES (?, ?, ?)')
  const count = () => Number(db.prepare('SELECT COUNT(*) AS n FROM steps').get().n)
  let step = Number(db.prepare('SELECT COALESCE(MAX(step_index), -1) AS n FROM steps').get().n) + 1
  let turns = 0

  write({
    event: 'init',
    conversation_id: conversation,
    init: {
      cwd: process.cwd(),
      model,
      tools: ['list_dir', 'view_file', 'grep_search', 'find_by_name', 'run_command', 'write_file', 'replace_file_content', 'read_url', 'call_mcp_tool', 'ask_question', 'invoke_subagent'],
      permission_mode: 'always-proceed',
    },
  })

  async function tool(name, parameters) {
    const index = step++
    const call = { name, args: parameters }
    write({
      event: 'step_update',
      step_update: {
        step_index: index,
        step_type: 'tool',
        state: 'ACTIVE',
        tool_name: name,
        tool_info: { name, parameters },
      },
    })
    const hook = await hooksAllow(call, { conversation })
    const permitted = hook.allowed && !deniedBySettings(settings, call)
    log({
      type: 'tool',
      tool: name,
      hook: hook.allowed,
      permitted,
    })
    let info
    if (!hook.allowed) {
      info = { name, parameters, error: { message: `tool call denied by pre-tool hook: ${hook.reason}` } }
    }
    else if (!permitted) {
      info = { name, parameters, error: { message: `Permission denied by settings for ${name}.` } }
    }
    else if (name === 'run_command') {
      const output = await new Promise((resolve) => {
        const child = spawn('sh', ['-c', parameters.CommandLine], { cwd: process.cwd(), stdio: ['ignore', 'pipe', 'pipe'] })
        let text = ''
        child.stdout.on('data', chunk => text += chunk)
        child.stderr.on('data', chunk => text += chunk)
        child.on('close', () => resolve(text))
      })
      info = { name, parameters, output }
    }
    else if (name === 'write_file') {
      writeFileSync(parameters.Path, parameters.Content)
      info = { name, parameters, output: `Created ${parameters.Path}` }
    }
    else {
      info = { name, parameters, output: readdirSync(parameters.DirectoryPath).sort().join('\n') }
    }

    write({
      event: 'step_update',
      step_update: {
        step_index: index,
        step_type: 'tool',
        state: info.error ? 'ERROR' : 'DONE',
        tool_name: name,
        tool_info: info,
      },
    })
  }

  async function answer(content) {
    turns++
    const earlier = count()
    const userIndex = step++
    insert.run(userIndex, 'user', content)
    write({
      event: 'step_update',
      step_update: {
        step_index: userIndex,
        step_type: 'user_input',
        state: 'DONE',
        text: content,
      },
    })
    const result = fields => write({ event: 'result', result: { conversation_id: conversation, num_turns: turns, ...fields } })

    if (content.includes('fixture:agy-command')) {
      // A command `agy` started, in its process group, that only a group kill stops.
      // Like agy 1.5.14, each command runs in its own session.
      const command = spawn('sleep', ['300'], { stdio: 'ignore', detached: true })
      writeFileSync(path.join(home, 'agy-command.pid'), String(command.pid))
    }

    const slow = /fixture:agy-slow=(\d+)/.exec(content)
    if (slow)
      await new Promise(resolve => setTimeout(resolve, Number(slow[1])))
    if (content.includes('fixture:agy-429')) {
      fail({ short_error: 'Resource has been exhausted (e.g. check quota).', status: 'RESOURCE_EXHAUSTED', error_code: 429 })
      result({ status: 'ERROR', error: 'Error 429: Resource has been exhausted (e.g. check quota).' })
      return
    }

    if (content.includes('fixture:agy-expired') && current.kind === 'google') {
      const key = `expired:${current.token.token.access_token}`
      if (!db.prepare('SELECT value FROM meta WHERE key = ?').get(key)) {
        db.prepare('INSERT INTO meta (key, value) VALUES (?, ?)').run(key, '1')
        fail({ short_error: 'request failed: oauth2: token expired and refresh token is not set', status: '', error_code: 0 })
        result({ status: 'ERROR', error: 'oauth2: token expired and refresh token is not set' })
        return
      }
    }

    if (content.includes('fixture:agy-tools')) {
      const cwd = process.cwd()
      await tool('list_dir', { DirectoryPath: cwd })
      await tool('run_command', {
        CommandLine: 'echo fixture-command > agy-command.txt && echo ran',
        Cwd: cwd,
        WaitMsBeforeAsync: 3000,
        toolSummary: 'Write a marker',
      })
      await tool('write_file', { Path: path.join(cwd, 'agy-written.txt'), Content: 'fixture file', Overwrite: true })
    }

    const asked = content.split('\n').map(line => line.trim()).filter(Boolean).at(-1) ?? ''
    const response = `I prepared a compact status panel. You asked: "${asked.slice(0, 160)}". Earlier steps in this conversation: ${earlier}.`
    const responseIndex = step++
    const middle = Math.floor(response.length / 2)
    write({
      event: 'step_update',
      step_update: {
        step_index: responseIndex,
        step_type: 'agent_response',
        state: 'ACTIVE',
        text_delta: response.slice(0, middle),
      },
    })
    write({
      event: 'step_update',
      step_update: {
        step_index: responseIndex,
        step_type: 'agent_response',
        state: 'ACTIVE',
        text_delta: response.slice(middle),
      },
    })
    write({
      event: 'step_update',
      step_update: {
        step_index: responseIndex,
        step_type: 'agent_response',
        state: 'DONE',
        text_delta: '',
      },
    })
    insert.run(responseIndex, 'agent', response)
    result({
      status: 'SUCCESS',
      response,
      usage: {
        input_tokens: 1200 + content.length,
        output_tokens: response.length,
        cached_input_tokens: 0,
        thinking_tokens: 16,
      },
    })
  }

  // Messages that arrive during a turn wait for it, in order.
  const queue = []
  let busy = false
  let closed = false
  const drain = async () => {
    if (busy)
      return
    busy = true
    while (queue.length)
      await answer(queue.shift())
    busy = false
    if (closed) {
      db.close()
      process.exit(0)
    }
  }

  const lines = createInterface({ input: process.stdin })
  lines.on('line', (line) => {
    let event
    try {
      event = JSON.parse(line)
    }
    catch {
      return
    }

    if (event.event !== 'user')
      return
    const content = typeof event.message?.content === 'string'
      ? event.message.content
      : (event.message?.content ?? []).filter(part => part.type === 'text').map(part => part.text).join('\n')
    log({ type: 'message', content })
    queue.push(content)
    drain()
  })
  lines.on('close', () => {
    closed = true
    drain()
  })
}

if (args.includes('--version')) {
  process.stdout.write(`${process.env.CAIRN_AGY_FIXTURE_VERSION || '1.5.14'}\n`)
  process.exit(0)
}
else if (args[0] === 'models') {
  models()
}
else if (args[0] === 'acp' && args.includes('--login')) {
  login()
}
else if (args.includes('-p=') || args.includes('-p')) {
  print()
}
else {
  process.stderr.write(`agy fixture: unsupported arguments ${JSON.stringify(args)}\n`)
  process.exit(2)
}
