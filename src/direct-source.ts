// Adapt the existing credited SSE body to the live client's EventSource seam.
export class DirectSource extends EventTarget {
  onerror: (() => void) | null = null
  private abort = new AbortController()

  constructor(request: (signal: AbortSignal) => Promise<Response>) {
    super()
    void this.read(request)
  }

  close() { this.abort.abort() }

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
    catch {
      if (!this.abort.signal.aborted)
        this.onerror?.()
    }
    finally { await reader?.cancel().catch(() => {}) }
  }
}
