import { once } from 'node:events'
import { connect } from 'node:net'
import { expect, it } from 'vitest'
import { mcpProvider } from '../../tests/fixtures/mcp-provider'

it('closes the provider even when a client keeps an HTTP connection open', async () => {
  const provider = await mcpProvider()
  const socket = connect(Number(new URL(provider.origin).port), '127.0.0.1')
  let closing: Promise<void> | undefined
  let deadline: ReturnType<typeof setTimeout> | undefined
  socket.on('error', () => { /* Server shutdown may reset an incomplete request. */ })
  try {
    await once(socket, 'connect')
    // Leave an incomplete request in flight, as a streaming client can do.
    socket.write('GET /mcp HTTP/1.1\r\nHost: localhost\r\n')
    const closed = new Promise<void>((resolve, reject) => {
      deadline = setTimeout(() => reject(new Error('Provider left its client connection open')), 1500)
      socket.once('close', () => resolve())
    })
    closing = provider.close()
    await closed
    await closing
    expect(socket.destroyed).toBe(true)
  }
  finally {
    clearTimeout(deadline)
    socket.destroy()
    await (closing || provider.close())
  }
})
