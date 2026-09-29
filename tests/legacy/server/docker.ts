import { Buffer } from 'node:buffer'
import http from 'node:http'

export function dockerRequest(method: string, endpoint: string, body?: unknown) {
  return new Promise<http.IncomingMessage>((resolve, reject) => {
    const request = http.request({
      socketPath: '/var/run/docker.sock',
      path: endpoint,
      method,
      headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    }, (response) => {
      if ((response.statusCode ?? 500) < 400) {
        resolve(response)
        return
      }

      response.resume()
      reject(Object.assign(new Error(`Runner container operation failed (${response.statusCode}).`), { statusCode: response.statusCode }))
    })
    request.on('error', reject)
    request.setTimeout(endpoint.endsWith('/wait') || endpoint.includes('/logs?') ? 0 : 30000, () => request.destroy(new Error('Container engine request timed out.')))
    request.end(body === undefined ? undefined : JSON.stringify(body))
  })
}

export async function dockerJson(method: string, endpoint: string, body?: unknown) {
  const response = await dockerRequest(method, endpoint, body)
  let result = ''
  for await (const chunk of response)
    result += chunk
  return result ? JSON.parse(result) : {}
}

export async function* dockerOutput(stream: AsyncIterable<Uint8Array>) {
  let buffer = Buffer.alloc(0)
  for await (const chunk of stream) {
    buffer = Buffer.concat([buffer, chunk])
    while (buffer.length >= 8) {
      const length = buffer.readUInt32BE(4)
      if (length > 16 * 1024 * 1024)
        throw new Error('Invalid container log frame.')
      if (buffer.length < length + 8)
        break
      yield { stderr: buffer[0] === 2, data: buffer.subarray(8, 8 + length) }
      buffer = buffer.subarray(8 + length)
    }
  }
}
