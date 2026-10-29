// Wire contract: cairn-protocol::{Frame, data_channel}, protocol v4.
// These limits/envelopes match the existing Rust dispatcher, never HTTP identity.
const MAX_BODY = 8_000_000
const MAX_FRAME = Math.ceil(MAX_BODY / 3) * 4 + 65536
const MAX_PACKET = 16384
const HEADER = 13
const encoder = new TextEncoder()

export class TransportLost extends Error {
  constructor() { super('Connection interrupted. Please check the result before trying again.') }
}

/** A wire-version refusal is terminal until the app and installation are updated. */
export class ProtocolMismatch extends Error {
  constructor(message = 'Protocol mismatch. Update required: update the app, Beacon and installation.') { super(message) }
}

// The peer could not dispatch this request: even mutations are safe on relay.
export class TransportNotSent extends Error {
  constructor() { super('Direct request was not sent.') }
}

interface Frame {
  type: string
  id?: string
  status?: number
  headers?: [string, string][]
  body?: string
  failed?: boolean
}

interface Pending {
  resolve: (response: Response) => void
  reject: (error: Error) => void
  timer: ReturnType<typeof setTimeout>
  stream?: ReadableStreamDefaultController<Uint8Array>
  started: boolean
  credited: boolean
  streaming: boolean
  head: boolean
  sent: boolean
  cleanup: () => void
}

function bytes(value: string, limit = MAX_BODY) {
  if (value.length > Math.ceil(limit / 3) * 4)
    throw new Error('Direct body too large')
  const decoded = Uint8Array.from(atob(value), char => char.charCodeAt(0))
  if (decoded.length > limit)
    throw new Error('Direct body too large')
  return decoded
}

function base64(value: Uint8Array) {
  let text = ''
  for (let start = 0; start < value.length; start += 16384)
    text += String.fromCharCode(...value.subarray(start, start + 16384))
  return btoa(text)
}

export class DirectChannel {
  private pending = new Map<string, Pending>()
  private assemblies = new Map<number, {
    total: number
    chunks: Uint8Array[]
    length: number
    started: number
  }>()

  private buffered = 0
  private transfer = 0
  private closed = false
  private writer = Promise.resolve()
  private outgoing = new Map<number, number>()
  private outgoingBytes = 0
  private outgoingWaiters = new Set<() => void>()
  private deadline: ReturnType<typeof setInterval>

  constructor(private channel: RTCDataChannel, private identity: { account_id: string, role: string }, private failed: (error?: unknown) => void) {
    channel.bufferedAmountLowThreshold = 32768
    channel.addEventListener('message', this.receive)
    this.deadline = setInterval(() => {
      if ([...this.assemblies.values()].some(assembly => Date.now() - assembly.started >= 30000))
        this.failed()
    }, 1000)
  }

  close(reason?: Error) {
    if (this.closed)
      return
    this.closed = true
    clearInterval(this.deadline)
    this.channel.removeEventListener('message', this.receive)
    for (const [id, pending] of this.pending) {
      this.release(id)
      if (pending.started)
        pending.stream?.error(reason ?? new TransportLost())
      else
        pending.reject(reason ?? (pending.sent ? new TransportLost() : new TransportNotSent()))
    }

    this.assemblies.clear()
    this.buffered = 0
    this.channel.dispatchEvent(new Event('bufferedamountlow'))
    for (const ready of this.outgoingWaiters)
      ready()
  }

  private release(id: string) {
    const pending = this.pending.get(id)
    if (!pending)
      return
    clearTimeout(pending.timer)
    pending.cleanup()
    this.pending.delete(id)
  }

  private async capacity(): Promise<void> {
    if (this.closed || this.channel.readyState !== 'open')
      throw new TransportLost()
    if (this.channel.bufferedAmount + MAX_PACKET <= 65536)
      return
    await new Promise<void>((resolve, reject) => {
      let timeout: ReturnType<typeof setTimeout>
      let ready: () => void
      const finish = () => {
        clearTimeout(timeout)
        this.channel.removeEventListener('bufferedamountlow', ready)
        this.channel.removeEventListener('close', ready)
        if (this.closed || this.channel.readyState !== 'open')
          reject(new TransportLost())
        else
          resolve()
      }

      ready = finish
      timeout = setTimeout(() => {
        finish()
        this.failed()
      }, 30000)
      this.channel.addEventListener('bufferedamountlow', ready)
      this.channel.addEventListener('close', ready)
      if (this.channel.bufferedAmount <= 32768 || this.closed)
        finish()
    })
    return this.capacity()
  }

  private async send(frame: object, valid = () => true, sent = () => {}) {
    const payload = encoder.encode(JSON.stringify(frame))
    if (payload.length > MAX_FRAME)
      throw new Error('Direct frame too large')
    const transfer = this.transfer = (this.transfer + 1) >>> 0 || 1
    // Reserve whole payloads conservatively: interleaving must never exceed the
    // existing peer's aggregate byte/transfer limits during reassembly.
    while (this.outgoing.size >= 32 || this.outgoingBytes + payload.length > MAX_FRAME) {
      if (this.closed)
        throw new TransportLost()
      if (!valid())
        return
      await new Promise<void>((resolve) => {
        const ready = () => {
          this.outgoingWaiters.delete(ready)
          resolve()
        }

        this.outgoingWaiters.add(ready)
      })
    }

    this.outgoing.set(transfer, payload.length)
    this.outgoingBytes += payload.length
    try {
      let offset = 0
      while (offset < payload.length) {
        // Serialize a single packet, then queue our next fragment behind any
        // reads, credits or heartbeat that arrived during backpressure.
        const operation = this.writer.then(async () => {
          await this.capacity()
          if (!valid()) {
            if (offset) {
              const abandon = new Uint8Array(HEADER)
              const header = new DataView(abandon.buffer)
              header.setUint8(0, 1)
              header.setUint32(1, transfer)
              this.channel.send(abandon)
            }

            return false
          }

          const length = Math.min(MAX_PACKET - HEADER, payload.length - offset)
          const packet = new Uint8Array(HEADER + length)
          const header = new DataView(packet.buffer)
          header.setUint8(0, 1)
          header.setUint32(1, transfer)
          header.setUint32(5, payload.length)
          header.setUint32(9, offset)
          packet.set(payload.subarray(offset, offset + length), HEADER)
          this.channel.send(packet)
          offset += length
          if (offset === payload.length)
            sent()
          return true
        })
        this.writer = operation.then(() => {}, () => this.failed())
        if (!await operation)
          return
      }
    }
    finally {
      this.outgoing.delete(transfer)
      this.outgoingBytes -= payload.length
      for (const ready of this.outgoingWaiters)
        ready()
    }
  }

  request(path: string, options: RequestInit = {}, onSent = () => {}) {
    if (this.closed || this.channel.readyState !== 'open')
      return Promise.reject(new TransportNotSent())
    const route = path.split('?')[0]
    const streaming = route.endsWith('/stream') || route.startsWith('/api/shared-artifacts/')
    const streams = [...this.pending.values()].filter(value => value.streaming).length
    if (this.pending.size >= 32 || (streaming && streams >= 8))
      return Promise.reject(new TransportNotSent())
    const body = typeof options.body === 'string' ? encoder.encode(options.body) : new Uint8Array()
    if (body.length > MAX_BODY)
      return Promise.resolve(Response.json({ error: 'Request body too large.' }, { status: 413 }))
    const id = crypto.randomUUID()
    return new Promise<Response>((resolve, reject) => {
      const abort = () => {
        const pending = this.pending.get(id)
        this.release(id)
        if (pending?.started)
          pending.stream?.error(new DOMException('Aborted', 'AbortError'))
        else
          reject(new DOMException('Aborted', 'AbortError'))
        void this.send({ type: 'cancel', id }).catch(() => {})
      }

      this.pending.set(id, {
        resolve,
        reject,
        streaming,
        head: options.method?.toUpperCase() === 'HEAD',
        sent: false,
        started: false,
        credited: false,
        cleanup: () => options.signal?.removeEventListener('abort', abort),
        timer: setTimeout(() => {
          const pending = this.pending.get(id)
          this.release(id)
          reject(pending?.sent ? new Error('Request timed out. Please check the result before trying again.') : new TransportNotSent())
          void this.send({ type: 'cancel', id }).catch(() => {})
        }, 35000),
      })
      options.signal?.addEventListener('abort', abort, { once: true })
      if (options.signal?.aborted) {
        abort()
        return
      }

      const headers = new Headers(options.headers)
      // DataChannel responses bypass the browser's HTTP decompression.
      headers.set('accept-encoding', !streaming && typeof DecompressionStream !== 'undefined' ? 'gzip' : 'identity')
      if (options.body !== undefined && !headers.has('content-type'))
        headers.set('content-type', 'application/json')
      const forwarded = [...headers].filter(([name]) => ['content-type', 'accept', 'accept-encoding', 'range', 'if-none-match', 'if-modified-since', 'last-event-id', 'mcp-protocol-version', 'mcp-method'].includes(name))
      void this.send({
        type: 'request',
        id,
        ...this.identity,
        method: options.method || 'GET',
        path,
        headers: forwarded,
        body: base64(body),
      }, () => this.pending.has(id), () => {
        const pending = this.pending.get(id)
        if (pending) {
          pending.sent = true
          onSent()
        }
      }).catch(() => {})
    })
  }

  private receive = (event: MessageEvent) => {
    try {
      if (!(event.data instanceof ArrayBuffer))
        throw new Error('Binary channel required')
      const packet = new Uint8Array(event.data)
      if (packet.length < HEADER || packet.length > MAX_PACKET)
        throw new Error('Invalid fragment')
      if (packet[0] !== 1)
        throw new ProtocolMismatch()
      const header = new DataView(packet.buffer)
      const id = header.getUint32(1)
      const total = header.getUint32(5)
      const offset = header.getUint32(9)
      const payload = packet.subarray(HEADER)
      if (!id || total > MAX_FRAME)
        throw new Error('Invalid transfer')
      if (!total && !offset && !payload.length) {
        this.buffered -= this.assemblies.get(id)?.length || 0
        this.assemblies.delete(id)
        return
      }

      if (!payload.length || offset + payload.length > total || this.buffered + payload.length > MAX_FRAME)
        throw new Error('Reassembly limit exceeded')
      if (!offset) {
        if (this.assemblies.has(id) || this.assemblies.size >= 32)
          throw new Error('Too many transfers')
        this.assemblies.set(id, {
          total,
          chunks: [],
          length: 0,
          started: Date.now(),
        })
      }

      const assembly = this.assemblies.get(id)
      if (!assembly || assembly.total !== total || assembly.length !== offset || Date.now() - assembly.started >= 30000)
        throw new Error('Invalid sequence')
      assembly.chunks.push(payload)
      assembly.length += payload.length
      this.buffered += payload.length
      if (assembly.length !== total)
        return
      this.assemblies.delete(id)
      this.buffered -= total
      const json = new Uint8Array(total)
      let position = 0
      for (const chunk of assembly.chunks) {
        json.set(chunk, position)
        position += chunk.length
      }

      this.dispatch(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(json)))
    }
    catch (error) { this.failed(error) }
  }

  private dispatch(frame: Frame) {
    if (frame.type === 'update_required')
      throw new ProtocolMismatch()
    if (!['response', 'stream_start', 'stream_chunk', 'stream_end'].includes(frame.type) || typeof frame.id !== 'string')
      throw new Error('Unexpected frame')
    const id = frame.id
    const pending = this.pending.get(id)
    if (!pending)
      return // Response to an abandoned request; never deliver to another caller.
    if (frame.type === 'response' || frame.type === 'stream_start') {
      if (pending.started || !Number.isInteger(frame.status) || frame.status! < 200 || frame.status! > 599 || !Array.isArray(frame.headers))
        throw new Error('Invalid response')
      const headers = new Headers(frame.headers)
      headers.set('x-cairn-transport', 'direct')
      if (frame.type === 'response') {
        const body = bytes(frame.body!)
        this.release(id)
        // Reserved transport refusal, emitted before dispatch and excluded from
        // application response headers. An ordinary application 503 is not safe
        // to replay, even if its body has the same wording.
        if (frame.status === 503 && headers.get('x-cairn-direct-rejection') === 'reassembly-busy') {
          pending.reject(new TransportNotSent())
          return
        }

        let response = new Response(pending.head || [204, 205, 304].includes(frame.status!) ? null : body, { status: frame.status, headers })
        if (headers.get('content-encoding') === 'gzip' && response.body) {
          let size = 0
          const decoded = response.body
            .pipeThrough(new DecompressionStream('gzip'))
            .pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
              transform(chunk, controller) {
                size += chunk.byteLength
                if (size > MAX_BODY)
                  throw new Error('Response is too large')
                controller.enqueue(chunk)
              },
            }))
          headers.delete('content-encoding')
          headers.delete('content-length')
          response = new Response(decoded, { status: frame.status, headers })
        }

        pending.resolve(response)
        return
      }

      clearTimeout(pending.timer)
      pending.started = true
      const body = new ReadableStream<Uint8Array>({
        start: controller => pending.stream = controller,
        pull: () => {
          if (this.pending.has(id) && !pending.credited) {
            pending.credited = true
            return this.send({ type: 'stream_credit', id })
          }
        },
        cancel: () => {
          this.release(id)
          return this.send({ type: 'cancel', id })
        },
      }, { highWaterMark: 0 })
      pending.resolve(new Response(body, { status: frame.status, headers }))
    }
    else if (frame.type === 'stream_chunk') {
      if (!pending.started || !pending.credited)
        throw new Error('Uncredited chunk')
      pending.credited = false
      pending.stream!.enqueue(bytes(frame.body!, 65536))
    }
    else {
      if (!pending.started)
        throw new Error('Stream not started')
      this.release(id)
      if (frame.failed)
        pending.stream!.error(new TransportLost())
      else
        pending.stream!.close()
    }
  }
}
