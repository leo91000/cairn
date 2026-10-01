// Real Codex through the Rust adapter, isolated homes and synthetic model/MCP.
// Run: node tests/fixtures/ready-codex.mjs /path/to/leo /path/to/native/codex
import assert from 'node:assert/strict'
import { Buffer } from 'node:buffer'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import {
  access,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  writeFile,
} from 'node:fs/promises'
import { createServer } from 'node:http'
import { createConnection } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

async function main() {
  const [leo, codex] = process.argv.slice(2)
  assert.ok(leo && codex, 'Provide the Rust adapter and real native Codex executable')
  const root = await mkdtemp(path.join(os.tmpdir(), 'leo-ready-codex-'))
  const home = path.join(root, 'home')
  const nativeHome = path.join(home, '.codex')
  const workspace = path.join(root, 'workspace')
  const socket = path.join(root, 'service/codex.sock')
  const modelRequests = []
  const mcpRequests = []
  const live = new Set()
  let current
  let service
  let nativePid

  const server = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request)
      body += chunk
    const input = body ? JSON.parse(body) : {}
    if (request.url === '/mcp') {
      if (request.method !== 'POST') {
        response.writeHead(405).end()
        return
      }

      const bearer = request.headers.authorization
      assert.ok(['Bearer alpha', 'Bearer beta'].includes(bearer), 'Attempt-specific synthetic MCP lease')
      mcpRequests.push({ label: current.label, bearer, method: input.method })
      if (input.id === undefined) {
        response.writeHead(202).end()
        return
      }

      const tool = bearer === 'Bearer alpha' ? 'alpha_tool' : 'beta_tool'
      const result = input.method === 'initialize'
        ? { protocolVersion: input.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' } }
        : input.method === 'tools/list'
          ? { tools: [{ name: tool, description: 'Synthetic lease check', inputSchema: { type: 'object', properties: {} } }] }
          : { content: [{ type: 'text', text: 'synthetic tool result' }] }
      response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({ jsonrpc: '2.0', id: input.id, result }))
      return
    }

    if (request.method !== 'POST' || !request.url.endsWith('/responses')) {
      response.writeHead(404, { 'content-type': 'application/json' }).end('{}')
      return
    }

    modelRequests.push({
      label: current.label,
      at: performance.now(),
      tools: JSON.stringify(input.tools),
      input: JSON.stringify(input.input),
      keys: Object.keys(input),
    })
    const item = {
      id: `msg_${randomUUID()}`,
      type: 'message',
      role: 'assistant',
      status: 'completed',
      content: [{ type: 'output_text', text: 'READY_ADAPTER_OK', annotations: [] }],
    }
    const result = {
      id: `resp_${randomUUID()}`,
      object: 'response',
      status: 'completed',
      output: [item],
      usage: {
        input_tokens: 10,
        output_tokens: 3,
        total_tokens: 13,
        input_tokens_details: { cached_tokens: 0 },
        output_tokens_details: { reasoning_tokens: 0 },
      },
    }
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    for (const event of [
      { type: 'response.created', response: { ...result, status: 'in_progress', output: [] } },
      { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', content: [] } },
      {
        type: 'response.output_text.delta',
        item_id: item.id,
        output_index: 0,
        content_index: 0,
        delta: 'READY_ADAPTER_OK',
      },
      { type: 'response.output_item.done', output_index: 0, item },
      { type: 'response.completed', response: result },
    ])
      response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`)
    response.end()
  })

  function launch(args, environment) {
    const child = spawn(leo, args, { cwd: workspace, env: environment, stdio: ['pipe', 'pipe', 'pipe'] })
    live.add(child)
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', data => stdout += data)
    child.stderr.on('data', data => stderr += data)
    const done = new Promise((resolve, reject) => {
      child.once('error', reject)
      child.once('exit', (code) => {
        live.delete(child)
        resolve(code)
      })
    })
    return {
      child,
      done,
      stdout: () => stdout,
      stderr: () => stderr,
    }
  }

  async function ready() {
    return new Promise((resolve) => {
      const client = createConnection(socket)
      const plan = Buffer.from(JSON.stringify({ op: 'ready' }))
      const header = Buffer.alloc(4)
      header.writeUInt32BE(plan.length)
      let reply = Buffer.alloc(0)
      const timer = globalThis.setTimeout(() => {
        client.destroy()
        resolve(false)
      }, 30000)
      client.once('error', () => {
        globalThis.clearTimeout(timer)
        resolve(false)
      })
      client.once('connect', () => client.write(Buffer.concat([header, plan])))
      client.on('data', (bytes) => {
        reply = Buffer.concat([reply, bytes])
        if (reply.length < 4 || reply.length < 4 + reply.readUInt32BE(0))
          return
        globalThis.clearTimeout(timer)
        client.end()
        resolve(JSON.parse(reply.subarray(4).toString()).ready)
      })
    })
  }

  try {
    await mkdir(nativeHome, { recursive: true })
    await mkdir(workspace)
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
    const endpoint = `http://127.0.0.1:${server.address().port}`
    await writeFile(path.join(nativeHome, 'config.toml'), `model_provider = "fixture"\nmodel = "gpt-5.4"\n[model_providers.fixture]\nname = "Fixture"\nbase_url = "${endpoint}"\nwire_api = "responses"\nrequires_openai_auth = false\n`)
    const environment = {
      PATH: process.env.PATH,
      HOME: home,
      AGENT_HOME: home,
      CODEX_HOME: nativeHome,
      CODEX_BIN: codex,
      NO_COLOR: '1',
      RUST_LOG: 'warn,leo_performance=info',
    }
    const samples = []
    let thread

    async function turn(label, bearer, resident, resume) {
      current = { label, started: performance.now() }
      const mcp = bearer ? { fixture: { url: `${endpoint}/mcp`, http_headers: { Authorization: bearer } } } : {}
      const plan = {
        provider: 'codex',
        execution: { messageId: randomUUID(), text: `Remember ${label}, and reply READY_ADAPTER_OK.`, recovery: false },
        instructions: 'Reply with the requested marker.',
        inputDirectory: root,
        output: path.join(root, `${label}.md`),
        cwd: workspace,
        model: 'gpt-5.4',
        reasoning: 'low',
        sandbox: 'yolo',
        writableRoots: [workspace],
        codexConfig: { mcp_servers: mcp },
        args: resident || !bearer ? [] : ['-c', `mcp_servers.fixture={url="${endpoint}/mcp",http_headers={Authorization="${bearer}"}}`],
        ...(resume ? { sessionId: thread } : {}),
      }
      const launched = launch(['chat', codex], { ...environment, ...(resident ? { LEO_CODEX_SERVICE: socket } : {}) })
      launched.child.stdin.end(JSON.stringify(plan))
      const code = await launched.done
      assert.equal(code, 0, launched.stderr().slice(-5000))
      const events = launched.stdout().trim().split('\n').map(line => JSON.parse(line))
      assert.ok(events.some(event => event.type === 'turn.completed'), 'Native turn completed')
      const opened = events.find(event => event.type === 'thread.started')?.thread_id
      assert.ok(opened)
      if (resume)
        assert.equal(opened, thread)
      thread = opened
      assert.match(await readFile(plan.output, 'utf8'), /READY_ADAPTER_OK/)
      const model = modelRequests.find(value => value.label === label && value.tools)
      assert.ok(model, `No tool-bearing model request: ${JSON.stringify({ requests: modelRequests.filter(value => value.label === label).map(value => ({ keys: value.keys, hasPrompt: value.input.includes(label) })), mcp: mcpRequests.filter(value => value.label === label) })}`)
      if (bearer) {
        const selected = bearer === 'Bearer alpha' ? 'alpha_tool' : 'beta_tool'
        assert.ok(model.tools.includes(selected), `${label}: current MCP tools loaded`)
        assert.ok(!model.tools.includes(selected === 'alpha_tool' ? 'beta_tool' : 'alpha_tool'), `${label}: previous tools absent`)
        assert.ok(mcpRequests.some(value => value.label === label && value.bearer === bearer && value.method === 'tools/list'))
      }
      else {
        assert.ok(!model.tools.includes('alpha_tool') && !model.tools.includes('beta_tool'), 'Removed MCP access stays removed')
      }

      if (resume)
        assert.match(model.input, /READY_ADAPTER_OK/, 'Earlier context was recovered')

      if (resident) {
        const children = (await readFile(`/proc/${service.child.pid}/task/${service.child.pid}/children`, 'utf8')).trim().split(/\s+/).filter(Boolean)
        assert.deepEqual(children, [nativePid], 'The same native process survives every lease')
      }

      samples.push({
        label,
        preModelMs: model.at - current.started,
        totalMs: performance.now() - current.started,
        contextPreserved: resume || null,
      })
    }

    await turn('cold', 'Bearer alpha', false, false)
    const started = performance.now()
    service = launch(['codex-service', socket], environment)
    service.child.stdin.end()
    while (!await ready()) {
      assert.ok(live.has(service.child), service.stderr().slice(-3000))
      assert.ok(performance.now() - started < 30000, 'Native initialization deadline')
      await setTimeout(10)
    }

    const initializeMs = performance.now() - started
    const children = (await readFile(`/proc/${service.child.pid}/task/${service.child.pid}/children`, 'utf8')).trim().split(/\s+/).filter(Boolean)
    assert.equal(children.length, 1)
    nativePid = children[0]
    await turn('ready-first', 'Bearer alpha', true, false)
    await turn('ready-resume', 'Bearer beta', true, true)
    await turn('ready-remove-mcp', null, true, true)
    service.child.kill('SIGTERM')
    assert.equal(await service.done, 0, service.stderr().slice(-3000))
    await assert.rejects(access(`/proc/${nativePid}`), { code: 'ENOENT' })
    process.stdout.write(`${JSON.stringify({
      kind: 'real-rust-ready-codex-adapter',
      scope: 'Local real native, synthetic model/MCP, no real account, no VM/manager/S3. Ready admission excludes prewarm.',
      initializeMs,
      samples,
      mcpLeasesRenewed: true,
      removedMcpAbsent: true,
      sameNativeProcess: true,
      nativeReapedOnShutdown: true,
    }, null, 2)}\n`)
  }
  finally {
    for (const child of live)
      child.kill('SIGTERM')
    await Promise.all([...live].map(child => new Promise(resolve => child.once('exit', resolve))))
    server.closeAllConnections()
    server.close()
    await rm(root, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
