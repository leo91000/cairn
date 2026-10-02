import { randomUUID } from 'node:crypto'
// Synthetic peer outside the guest: snapshot tests exercise the renewed network.
import { createServer } from 'node:http'

const records = []

createServer(async (request, response) => {
  if (request.method === 'GET' && request.url === '/records') {
    response.setHeader('content-type', 'application/json')
    response.end(JSON.stringify(records))
    return
  }

  if (request.method !== 'POST' || !request.url.endsWith('/responses')) {
    response.writeHead(404).end('{}')
    return
  }

  let raw = ''
  for await (const chunk of request)
    raw += chunk
  const body = JSON.parse(raw)
  const input = JSON.stringify(body.input)
  const marker = input.match(/snapshot-[a-f0-9-]{36}/)?.[0]
  if (!marker) {
    response.writeHead(400).end('{}')
    return
  }

  const verify = input.includes('VERIFY_SNAPSHOT')
  const success = verify ? 'SNAPSHOT_TOOL_VERIFIED' : 'SNAPSHOT_TOOL_EXECUTED'
  const outputs = body.input.filter(item => item.type === 'function_call_output')
  const executed = JSON.stringify(outputs.at(-1) || {}).includes(success)
  const tool = body.tools?.find(item => item.name === 'exec_command')
  records.push({
    at: Date.now(),
    marker,
    verify,
    executed,
    tools: Boolean(tool),
  })
  if (!executed && !tool) {
    response.writeHead(400).end('{}')
    return
  }

  const paths = ['snapshot-sentinel', '/home/node/.codex/snapshot-system-marker']
  const operation = verify
    ? `for(const path of paths)assert.equal(fs.readFileSync(path,'utf8'),marker);`
    : `for(const path of paths){fs.writeFileSync(path,marker);const fd=fs.openSync(path,'r');fs.fsyncSync(fd);fs.closeSync(fd);}const directory=fs.openSync('.','r');fs.fsyncSync(directory);fs.closeSync(directory);`
  const program = `const fs=require('node:fs'),assert=require('node:assert/strict');const marker=${JSON.stringify(marker)},paths=${JSON.stringify(paths)};${operation}console.log('${success}');`
  const quoted = `'${program.replaceAll('\'', '\'\\\'\'')}'`
  const item = executed
    ? {
        id: `msg_${randomUUID()}`,
        type: 'message',
        role: 'assistant',
        status: 'completed',
        content: [{ type: 'output_text', text: 'SNAPSHOT_NATIVE_OK', annotations: [] }],
      }
    : {
        id: `fc_${randomUUID()}`,
        type: 'function_call',
        status: 'completed',
        call_id: `call_${randomUUID()}`,
        name: tool.name,
        arguments: JSON.stringify({ cmd: `node -e ${quoted}`, login: false, yield_time_ms: 1000 }),
      }
  // Keep four real VMMs active long enough to inspect physical memory.
  if (executed && !verify && input.includes('HOLD_SNAPSHOT'))
    await new Promise(resolve => setTimeout(resolve, 12000))
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
    { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', ...(executed ? { content: [] } : { arguments: '' }) } },
    executed
      ? {
          type: 'response.output_text.delta',
          item_id: item.id,
          output_index: 0,
          content_index: 0,
          delta: 'SNAPSHOT_NATIVE_OK',
        }
      : {
          type: 'response.function_call_arguments.delta',
          item_id: item.id,
          output_index: 0,
          delta: item.arguments,
        },
    { type: 'response.output_item.done', output_index: 0, item },
    { type: 'response.completed', response: result },
  ])
    response.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`)
  response.end()
}).listen(8080, '0.0.0.0')
