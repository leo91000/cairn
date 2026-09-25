import { Buffer } from 'node:buffer'
import { lookup } from 'node:dns/promises'
import http from 'node:http'
import https from 'node:https'
import { BlockList, isIP } from 'node:net'
import { Readable, Transform } from 'node:stream'

const privateRanges = new BlockList()
const metadataAddresses = new BlockList()
metadataAddresses.addAddress('169.254.169.254', 'ipv4')
metadataAddresses.addAddress('fd00:ec2::254', 'ipv6')
for (const [address, bits] of [['0.0.0.0', 8], ['10.0.0.0', 8], ['100.64.0.0', 10], ['127.0.0.0', 8], ['169.254.0.0', 16], ['172.16.0.0', 12], ['192.168.0.0', 16], ['192.0.0.0', 24], ['198.18.0.0', 15], ['224.0.0.0', 4], ['240.0.0.0', 4]] as const)
  privateRanges.addSubnet(address, bits, 'ipv4')
for (const [address, bits] of [['::', 128], ['::1', 128], ['fc00::', 7], ['fe80::', 10], ['ff00::', 8]] as const)
  privateRanges.addSubnet(address, bits, 'ipv6')
export function isPrivateAddress(address: string) {
  return privateRanges.check(address, isIP(address) === 6 ? 'ipv6' : 'ipv4')
}

// DNS validation and the connection use the same resolved IP; redirects never bypass it.
export function mcpFetch(allowPrivateNetwork: boolean): typeof fetch {
  return async (input, init) => {
    const request = new Request(input, init)
    const url = new URL(request.url)
    const hostname = url.hostname.replace(/^\[|\]$/g, '')
    if (!['https:', 'http:'].includes(url.protocol) || url.username || url.password)
      throw new Error('Unsupported MCP endpoint.')
    const addresses = isIP(hostname) ? [{ address: hostname, family: isIP(hostname) }] : await lookup(hostname, { all: true })
    if (!addresses.length || (!allowPrivateNetwork && (url.protocol !== 'https:' || addresses.some(item => isPrivateAddress(item.address)))))
      throw new Error('Private network access is disabled for this connection.')
    // Cloud instance metadata must never receive connection credentials.
    if (addresses.some(item => metadataAddresses.check(item.address, item.family === 6 ? 'ipv6' : 'ipv4')))
      throw new Error('Instance metadata endpoints are unavailable.')
    const target = addresses[0]
    const body = request.body ? Buffer.from(await request.arrayBuffer()) : undefined
    const signal = AbortSignal.any([request.signal, AbortSignal.timeout(60000)])
    return await new Promise<Response>((resolve, reject) => {
      const req = (url.protocol === 'https:' ? https : http).request(url, {
        method: request.method,
        headers: Object.fromEntries(request.headers),
        signal,
        family: target.family,
        lookup: (_name, _options, callback) => callback(null, target.address, target.family),
      }, (res) => {
        if ((res.statusCode || 0) >= 300 && (res.statusCode || 0) < 400) {
          res.destroy()
          reject(new Error('The endpoint redirects. Configure its final URL.'))
          return
        }

        const headers = new Headers()
        for (const [key, value] of Object.entries(res.headers)) {
          if (value !== undefined)
            headers.set(key, Array.isArray(value) ? value.join(', ') : value)
        }

        let size = 0
        const limit = new Transform({
          transform(chunk, _encoding, callback) {
            size += chunk.length
            callback(size > 8 * 1024 * 1024 ? new Error('MCP response exceeds 8 MB.') : null, chunk)
          },
        })
        res.on('error', error => limit.destroy(error))
        limit.on('close', () => res.destroy())
        res.pipe(limit)
        const status = res.statusCode || 502
        resolve(new Response([204, 205, 304].includes(status) || request.method === 'HEAD' ? null : Readable.toWeb(limit) as ReadableStream, { status, headers }))
        if ([204, 205, 304].includes(status) || request.method === 'HEAD')
          res.resume()
      })
      req.on('error', reject)
      req.end(body)
    })
  }
}
