const crypto = require('node:crypto')
const http = require('node:http')

const requests = []
http.createServer(async (req, res) => {
  if (req.url === '/records') {
    res.setHeader('content-type', 'application/json')
    res.end(JSON.stringify(requests))
    return
  }

  if (req.method === 'GET') {
    res.setHeader('content-type', 'application/json')
    res.end('{}')
    return
  }

  let raw = ''
  for await (const chunk of req)
    raw += chunk

  const body = raw ? JSON.parse(raw) : {}
  if (req.url === '/mcp') {
    if (req.method !== 'POST') {
      res.writeHead(405).end()
      return
    }

    const lease = req.headers.authorization
    if (!['Bearer alpha', 'Bearer beta'].includes(lease)) {
      res.writeHead(403).end()
      return
    }

    if (body.id === undefined) {
      res.writeHead(202).end()
      return
    }

    const result = body.method === 'initialize'
      ? { protocolVersion: body.params.protocolVersion, capabilities: { tools: {} }, serverInfo: { name: 'fixture', version: '1' } }
      : body.method === 'tools/list' ? { tools: [{ name: lease === 'Bearer alpha' ? 'alpha_tool' : 'beta_tool', description: 'Synthetic lease', inputSchema: { type: 'object', properties: {} } }] } : { content: [] }
    res.setHeader('content-type', 'application/json')
    res.end(JSON.stringify({ jsonrpc: '2.0', id: body.id, result }))
    return
  }

  if (!req.url.endsWith('/responses') || req.method !== 'POST') {
    res.writeHead(404).end('{}')
    return
  }

  const tools = JSON.stringify(body.tools)
  const input = JSON.stringify(body.input)
  requests.push({ at: Date.now(), tools: body.tools?.length ? ['alpha_tool', 'beta_tool'].filter(name => tools.includes(name)).join(',') || 'no-mcp' : null, labels: [...new Set(input.match(/lease-\d+/g) || [])] })
  await new Promise(resolve => setTimeout(resolve, 1500))
  const item = {
    id: `msg_${crypto.randomUUID()}`,
    type: 'message',
    role: 'assistant',
    status: 'completed',
    content: [{ type: 'output_text', text: 'RETENTION_NATIVE_OK', annotations: [] }],
  }
  const response = {
    id: `resp_${crypto.randomUUID()}`,
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
  res.setHeader('content-type', 'text/event-stream')
  for (const event of [
    { type: 'response.created', response: { ...response, status: 'in_progress', output: [] } },
    { type: 'response.output_item.added', output_index: 0, item: { ...item, status: 'in_progress', content: [] } },
    {
      type: 'response.output_text.delta',
      item_id: item.id,
      output_index: 0,
      content_index: 0,
      delta: 'RETENTION_NATIVE_OK',
    },
    { type: 'response.output_item.done', output_index: 0, item },
    { type: 'response.completed', response },
  ])
    res.write(`event: ${event.type}\ndata: ${JSON.stringify(event)}\n\n`)

  res.end()
}).listen(8080, '0.0.0.0')
