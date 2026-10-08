import { TransportNotSent } from './direct-channel'

// Adapt the existing credited SSE body to the live client's EventSource seam.
export class DirectSource extends EventTarget {
  transportRoute: 'direct' | 'relay' = 'direct'
  onerror: (() => void) | null = null
  private abort = new AbortController()
  private relay?: EventSource

  constructor(request: (signal: AbortSignal) => Promise<Response>, private fallback?: () => EventSource) {
    super()
    void this.read(request)
  }

  close() {
    this.abort.abort()
    this.relay?.close()
  }

  private async read(request: (signal: AbortSignal) => Promise<Response>) {
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined
    try {
      const response = await request(this.abort.signal)
      if (!response.ok || !response.body)
        throw new Error('Stream unavailable')
      reader = response.body.getReader()
      const decoder = new TextDecoder('utf-8', { fatal: true })
      let buffer = ''
      let type = ''
      let id = ''
      let data: string[] = []
      let size = 0
      while (!this.abort.signal.aborted) {
        const { value, done } = await reader.read()
        if (done)
          throw new Error('Stream ended')
        buffer += decoder.decode(value, { stream: true })
        if (buffer.length + size > 8_000_000)
          throw new Error('SSE event too large')
        let newline = buffer.indexOf('\n')
        while (newline !== -1) {
          const line = buffer.slice(0, newline).replace(/\r$/, '')
          buffer = buffer.slice(newline + 1)
          if (!line) {
            if (data.length)
              this.dispatchEvent(new MessageEvent(type || 'message', { data: data.join('\n'), lastEventId: id }))
            type = ''
            data = []
            size = 0
          }
          else if (!line.startsWith(':')) {
            const colon = line.indexOf(':')
            const field = colon === -1 ? line : line.slice(0, colon)
            const value = colon === -1 ? '' : line.slice(colon + 1).replace(/^ /, '')
            if (field === 'event') {
              type = value
            }
            else if (field === 'id' && !value.includes('\0')) {
              id = value
            }
            else if (field === 'data') {
              data.push(value)
              size += value.length
            }
          }

          if (this.abort.signal.aborted)
            return
          newline = buffer.indexOf('\n')
        }
      }
    }
    catch (error) {
      if (!this.abort.signal.aborted) {
        if (error instanceof TransportNotSent && this.fallback) {
          this.transportRoute = 'relay'
          const relay = this.relay = this.fallback()
          for (const type of ['batch', 'ping', 'message']) {
            relay.addEventListener(type, (event) => {
              if (!this.abort.signal.aborted)
                this.dispatchEvent(new MessageEvent(type, { data: event.data, lastEventId: event.lastEventId }))
            })
          }

          relay.onerror = () => this.onerror?.()
        }
        else {
          this.onerror?.()
        }
      }
    }
    finally { await reader?.cancel().catch(() => {}) }
  }
}
