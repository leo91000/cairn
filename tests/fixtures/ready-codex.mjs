// Real Codex through the Rust adapter, isolated homes and synthetic model/MCP.
// Run: node tests/fixtures/ready-codex.mjs /path/to/cairn /path/to/native/codex
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
import { createServer as createBroker, createConnection } from 'node:net'
import os from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { setTimeout } from 'node:timers/promises'

async function main() {
  const [adapter, native] = process.argv.slice(2)
  assert.ok(adapter && native, 'Provide the Rust adapter and real native Codex executable')
  const cairn = path.resolve(adapter)
  const codex = path.resolve(native)
  const root = await mkdtemp(path.join(os.tmpdir(), 'cairn-ready-codex-'))
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
  let broker
  let credentialsIssued = 0
  const runId = randomUUID()

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
      atUnixMs: Date.now(),
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
    const child = spawn(cairn, args, { cwd: workspace, env: environment, stdio: ['pipe', 'pipe', 'pipe'] })
    live.add(child)
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', data => stdout += data)
    child.stderr.on('data', data => stderr += data)
    const done = new Promise((resolve, reject) => {
      child.once('error', (error) => {
        live.delete(child)
        reject(error)
      })
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
      RUST_LOG: 'warn,cairn_performance=debug',
    }
    const samples = []
    let thread

    async function turn(label, bearer, resident, resume) {
      current = { label, started: performance.now() }
      const mcp = bearer ? { fixture: { url: `${endpoint}/mcp`, http_headers: { Authorization: bearer } } } : {}
      const plan = {
        runId,
        attemptId: randomUUID(),
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
        codexConfig: {
          model_provider: 'fixture',
          model_providers: {
            fixture: {
              name: 'Fixture',
              base_url: endpoint,
              wire_api: 'responses',
              requires_openai_auth: false,
            },
          },
          mcp_servers: mcp,
        },
        args: resident || !bearer ? [] : ['-c', `mcp_servers.fixture={url="${endpoint}/mcp",http_headers={Authorization="${bearer}"}}`],
        ...(resume ? { sessionId: thread } : {}),
      }
      const launched = launch(['chat', codex], { ...environment, ...(resident ? { CAIRN_CODEX_SERVICE: socket } : {}) })
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
      if (resident)
        assert.ok(model.input.includes('pool_ready_skill'), 'Native discovers skills installed after anonymous warmup')
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
        assert.ok(service.stderr().split('\n').some(line => line.includes('codex_rpc') && line.includes(plan.attemptId)), 'Resident native RPC timing retains its attempt correlation')
      }

      samples.push({
        label,
        preModelMs: model.at - current.started,
        totalMs: performance.now() - current.started,
        contextPreserved: resume || null,
      })
    }

    await turn('cold', 'Bearer alpha', false, false)
    // The real guest warms before importing a conversation. Thread overrides
    // must therefore work without a provider or account in startup config.
    await rm(path.join(nativeHome, 'config.toml'))
    await writeFile(path.join(nativeHome, 'config.toml'), 'cli_auth_credentials_store = "file"\n')
    const started = performance.now()
    service = launch(['codex-service', socket], environment)
    service.child.stdin.end()
    while (!service.stdout().includes('\n')) {
      assert.ok(live.has(service.child), service.stderr().slice(-3000))
      assert.ok(performance.now() - started < 30000, 'Native initialization deadline')
      await setTimeout(10)
    }

    assert.equal(service.stdout(), '{"ready":true}\n', 'Native readiness has an explicit bounded frame')
    assert.equal(await ready(), true, 'Only initialized native accepts socket leases')

    const initializeMs = performance.now() - started
    const children = (await readFile(`/proc/${service.child.pid}/task/${service.child.pid}/children`, 'utf8')).trim().split(/\s+/).filter(Boolean)
    assert.equal(children.length, 1)
    nativePid = children[0]
    const skill = path.join(home, '.agents/skills/pool-ready')
    await mkdir(skill, { recursive: true })
    await writeFile(path.join(skill, 'SKILL.md'), '---\nname: pool_ready_skill\ndescription: A synthetic skill installed after anonymous warmup.\n---\nReply with the requested marker.\n')
    // Authentication is attached only after anonymous initialization. Exercise
    // the real native external-token login/logout without any real account.
    const claims = Buffer.from(JSON.stringify({
      'sub': 'synthetic-pool-account',
      'exp': Math.floor(Date.now() / 1000) + 3600,
      'https://api.openai.com/auth': { chatgpt_account_id: 'synthetic-pool-account', chatgpt_plan_type: 'plus' },
    })).toString('base64url')
    const accessToken = `eyJhbGciOiJub25lIn0.${claims}.synthetic`
    broker = createBroker((client) => {
      let input = ''
      client.on('data', (bytes) => {
        input += bytes
        if (!input.includes('\n'))
          return
        JSON.parse(input.trim())
        credentialsIssued++
        client.end(`${JSON.stringify({ accessToken, chatgptAccountId: 'synthetic-pool-account', chatgptPlanType: 'plus' })}\n`)
      })
    })
    await new Promise(resolve => broker.listen(path.join(nativeHome, 'cairn-auth.sock'), resolve))
    await writeFile(path.join(nativeHome, 'cairn-managed-auth'), '1')
    await turn('ready-first', 'Bearer alpha', true, false)
    await turn('ready-resume', 'Bearer beta', true, true)
    await turn('ready-remove-mcp', null, true, true)
    assert.ok(credentialsIssued >= 3, 'Every resident attempt acquires its own account access')
    const exported = (request) => {
      const transport = service.stderr().split('\n').filter(line => line.includes('operation="codex_transport"') && line.includes('endpoint="responses"'))
      return transport.some((line) => {
        const started = Number(line.match(/request_started_at_ms=(\d+)/)?.[1])
        const completed = Number(line.match(/completed_at_ms=(\d+)/)?.[1])
        return started <= request.atUnixMs + 100 && completed >= request.atUnixMs - 100
      })
    }

    const requests = modelRequests.filter(request => request.label !== 'cold')
    // The native SDK batches asynchronously. Test its export while it is alive;
    // SIGTERM can drop its final batch and must not delay production shutdown.
    const exportDeadline = performance.now() + 5000
    const lastRequest = requests.at(-1)
    assert.ok(lastRequest)
    while (!exported(lastRequest) && performance.now() < exportDeadline)
      await setTimeout(10)
    assert.ok(exported(lastRequest), 'Live native exports its last model request asynchronously')
    const transport = service.stderr().split('\n').filter(line => line.includes('operation="codex_transport"') && line.includes('endpoint="responses"'))
    for (const line of transport) {
      const started = Number(line.match(/request_started_at_ms=(\d+)/)?.[1])
      const completed = Number(line.match(/completed_at_ms=(\d+)/)?.[1])
      assert.ok(requests.some(request => started <= request.atUnixMs + 100 && completed >= request.atUnixMs - 100), 'Every reported native interval brackets a real model request, independently of export batching')
    }

    service.child.kill('SIGTERM')
    assert.equal(await service.done, 0, service.stderr().slice(-3000))
    assert.ok(!service.stderr().includes('Bearer alpha') && !service.stderr().includes('Bearer beta'), 'Instrumentation never records gateway credentials')

    await assert.rejects(access(`/proc/${nativePid}`), { code: 'ENOENT' })
    process.stdout.write(`${JSON.stringify({
      kind: 'real-rust-ready-codex-adapter',
      scope: 'Local real native, synthetic account/model/MCP, no real account, no VM/manager/S3. Resident starts without provider/account config and loads per-thread overrides and skills. Each attempt attaches managed external tokens. Ready admission excludes prewarm.',
      initializeMs,
      samples,
      mcpLeasesRenewed: true,
      removedMcpAbsent: true,
      sameNativeProcess: true,
      nativeReapedOnShutdown: true,
      nativeRequestTimingsObserved: true,
      syntheticAccountLeases: credentialsIssued,
    }, null, 2)}\n`)
  }
  finally {
    for (const child of live)
      child.kill('SIGTERM')
    await Promise.all([...live].map(child => new Promise(resolve => child.once('exit', resolve))))
    server.closeAllConnections()
    server.close()
    broker?.close()
    await rm(root, { recursive: true, force: true })
  }
}

main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
