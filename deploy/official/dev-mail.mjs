// Development mailbox only. Never use this adapter for a deployed service.
import { createServer } from 'node:http'

const messages = []
createServer(async (request, response) => {
  if (request.method === 'POST' && request.url === '/emails') {
    let body = ''
    for await (const chunk of request) {
      body += chunk
      if (body.length > 8192) {
        response.writeHead(413).end()
        return
      }
    }

    try {
      messages.unshift(JSON.parse(body))
      messages.splice(20)
      response.writeHead(200, { 'content-type': 'application/json' }).end('{"id":"development"}')
    }
    catch {
      response.writeHead(400).end()
    }

    return
  }

  response.writeHead(200, { 'content-type': 'application/json', 'cache-control': 'no-store' }).end(JSON.stringify(messages))
}).listen(8025, '0.0.0.0')
