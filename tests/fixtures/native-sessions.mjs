// Real provider CLIs, local model responses, private homes, no live credentials.
import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { randomUUID } from 'node:crypto'
import { mkdir, readFile, writeFile } from 'node:fs/promises'
import { createServer } from 'node:http'
import path from 'node:path'
import process from 'node:process'

async function main() {
  const mode = process.argv[2] || 'start'
  const root = path.resolve(process.argv[3] || '.')
  const home = path.join(root, 'native-home')
  await mkdir(path.join(home, '.codex'), { recursive: true })
  await mkdir(path.join(home, '.claude'), { recursive: true })
  const requests = []
  const server = createServer(async (request, response) => {
    let body = ''
    for await (const chunk of request) body += chunk
    const input = body ? JSON.parse(body) : {}
    requests.push(input)
    const reply = `native fixture reply ${mode}`
    if (request.url.includes('messages')) {
      const value = { id: `msg_${randomUUID()}`, type: 'message', role: 'assistant', model: input.model, content: [{ type: 'text', text: reply }], stop_reason: 'end_turn', stop_sequence: null, usage: { input_tokens: 10, output_tokens: 5 } }
      if (!input.stream) {
        response.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(value))
        return
      }
      response.writeHead(200, { 'content-type': 'text/event-stream' })
      const events = [
        { type: 'message_start', message: { ...value, content: [], stop_reason: null } },
        { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } },
        { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: reply } },
        { type: 'content_block_stop', index: 0 },
        { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 5 } },
        { type: 'message_stop' },
      ]
      for (const event of events) response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`)
      response.end()
      return
    }
    const item = { id: `msg_${randomUUID()}`, type: 'message', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: reply, annotations: [] }] }
    const result = { id: `resp_${randomUUID()}`, object: 'response', created_at: Math.floor(Date.now() / 1000), status: 'completed', model: 'fixture', output: [item], usage: { input_tokens: 10, output_tokens: 5, total_tokens: 15 } }
    response.writeHead(200, { 'content-type': 'text/event-stream' })
    for (const event of [
      { type: 'response.created', response: { ...result, status: 'in_progress', output: [] } },
      { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', content: [] } },
      { type: 'response.output_text.delta', item_id: item.id, output_index: 0, content_index: 0, delta: reply },
      { type: 'response.output_item.done', output_index: 0, item },
      { type: 'response.completed', response: result },
    ]) response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`)
    response.end()
  })
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  const endpoint = `http://127.0.0.1:${server.address().port}`
  const environment = { PATH: process.env.PATH, HOME: home, CODEX_HOME: path.join(home, '.codex'), CLAUDE_CONFIG_DIR: path.join(home, '.claude'), ANTHROPIC_API_KEY: 'synthetic-native-fixture', ANTHROPIC_BASE_URL: endpoint, CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: '1', DISABLE_TELEMETRY: '1', DISABLE_ERROR_REPORTING: '1', NO_COLOR: '1' }
  async function cli(binary, args) {
    const child = spawn(binary, args, { cwd: root, env: environment, stdio: ['ignore', 'pipe', 'pipe'] })
    let output = ''
    let error = ''
    child.stdout.on('data', data => output += data)
    child.stderr.on('data', data => error += data)
    const timer = setTimeout(() => child.kill('SIGKILL'), 60000)
    const code = await new Promise((resolve, reject) => {
      child.on('error', reject)
      child.on('exit', resolve)
    })
    clearTimeout(timer)
    assert.equal(code, 0, `${binary}: ${error.slice(-2000)} ${output.slice(-1000)}`)
    return output
  }
  try {
    const saved = mode === 'resume' ? JSON.parse(await readFile(path.join(root, 'native-sessions.json'), 'utf8')) : {}
    const prompt = mode === 'resume' ? 'Continue the native fixture conversation.' : 'Remember the initial-native-marker for our next turn.'
    const config = ['-c', 'model_provider="fixture"', '-c', 'model_providers.fixture.name="Fixture"', '-c', `model_providers.fixture.base_url="${endpoint}"`, '-c', 'model_providers.fixture.wire_api="responses"', '-c', 'model_providers.fixture.requires_openai_auth=false', '-m', 'fixture']
    const codexArgs = ['exec', ...config, '--json', '--skip-git-repo-check', ...(saved.codex ? ['resume', saved.codex] : []), prompt]
    const before = requests.length
    const codex = (await cli('codex', codexArgs)).trim().split('\n').map(line => JSON.parse(line))
    const codexId = codex.find(event => event.type === 'thread.started')?.thread_id
    assert.ok(codexId)
    assert.ok(codex.some(event => event.type === 'turn.completed'))
    if (saved.codex) {
      assert.equal(codexId, saved.codex)
      assert.match(JSON.stringify(requests.slice(before)), /initial-native-marker/)
    }
    const index = requests.length
    const claude = JSON.parse(await cli('claude', ['--bare', '-p', '--model', 'claude-sonnet-4-6', '--output-format', 'json', ...(saved.claude ? ['--resume', saved.claude] : []), prompt]))
    assert.equal(claude.is_error, false)
    assert.ok(claude.session_id)
    if (saved.claude) {
      assert.equal(claude.session_id, saved.claude)
      assert.match(JSON.stringify(requests.slice(index)), /initial-native-marker/)
    }
    await writeFile(path.join(root, 'native-sessions.json'), JSON.stringify({ codex: codexId, claude: claude.session_id }))
    process.stdout.write(`native.sessions.${mode}.verified\n`)
  }
  finally {
    server.closeAllConnections()
    server.close()
  }
}
main().catch((error) => {
  console.error(error)
  process.exitCode = 1
})
