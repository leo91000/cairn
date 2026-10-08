import { DirectChannel, TransportLost, TransportNotSent } from './direct-channel'
import { DirectSource } from './direct-source'
import { observeTransport } from './transport-observation'

export type TransportRoute = 'direct' | 'relay'

interface Context {
  installationId: string
  csrf: string
  authenticated: boolean
  ready: boolean
  transportRoute: TransportRoute
}

interface GrantResponse {
  available: boolean
  grant: {
    claims: {
      connection_id: string
      account_id: string
      role: 'owner' | 'member'
      expires_at: number
    }
  }
  iceServers: RTCIceServer[]
}

interface NetworkInformation extends EventTarget {
  type?: string
  effectiveType?: string
}

class SignalingError extends Error {
  constructor(readonly status: number) { super('Direct signaling unavailable') }
}

// Mirror #117's destination policy before signaling/SDK use. The authenticated
// Rust validator remains authoritative; no hostname resolution is introduced.
function usableCandidate(candidate: string) {
  if (!candidate)
    return true // End of candidates.
  const address = candidate.trim().split(/\s+/)[4]
  if (!address)
    return false
  const unicastV4 = (octets: number[]) => octets.length === 4 && octets.every(value => Number.isInteger(value) && value >= 0 && value <= 255)
    && octets[0] !== 127 && octets.some(value => value !== 0)
    && !(octets[0] === 169 && octets[1] === 254)
    && !(octets[0] >= 224 && octets[0] <= 239)
    && !octets.every(value => value === 255)
  if (/^\d+\.\d+\.\d+\.\d+$/.test(address))
    return unicastV4(address.split('.').map(Number))
  if (!address.includes(':'))
    return false
  try {
    const normalized = new URL(`http://[${address}]/`).hostname.slice(1, -1)
    const mapped = /^::ffff:([\da-f]+):([\da-f]+)$/.exec(normalized)
    if (mapped) {
      const high = Number.parseInt(mapped[1], 16)
      const low = Number.parseInt(mapped[2], 16)
      return unicastV4([high >> 8, high & 255, low >> 8, low & 255])
    }

    return normalized !== '::' && normalized !== '::1' && !/^(?:ff|fe[89ab])/i.test(normalized)
  }
  catch { return false }
}

function usableSdp(sdp: string) {
  return sdp.split('\r\n').filter(line => !line.startsWith('a=candidate:') || usableCandidate(line.slice(2))).join('\r\n')
}

// One transport per document/current installation. Signaling always uses HTTPS.
export class InstallationTransport {
  private peer?: RTCPeerConnection
  private channel?: RTCDataChannel
  private signals?: EventSource
  private timer?: ReturnType<typeof setTimeout>
  private expiry?: ReturnType<typeof setTimeout>
  private retry?: ReturnType<typeof setTimeout>
  private retryAt = 0
  private generation = 0
  private started = false
  private stopped = true
  private grant?: GrantResponse
  private traffic?: DirectChannel
  private handshake?: ReturnType<typeof setTimeout>
  private heartbeat?: ReturnType<typeof setTimeout>
  private controlAbort?: AbortController
  private hidden?: ReturnType<typeof setTimeout>
  private blocked = false
  private failures = 0
  private networkTimer?: ReturnType<typeof setTimeout>
  private network?: NetworkInformation
  private networkType?: string
  private effectiveType?: string
  private online?: boolean

  constructor(private readonly context: Context) {}

  private get base() {
    return `/api/installations/${encodeURIComponent(this.context.installationId)}/direct`
  }

  availabilityChanged(online?: boolean) {
    const recovered = this.online === false && online === true
    this.online = online
    if (online === false && this.started)
      this.fallback()
    else if (recovered)
      this.networkChanged()
  }

  private async control(path: string, body: unknown) {
    const response = await fetch(`${this.base}${path}`, {
      method: 'POST',
      signal: this.controlAbort?.signal,
      headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': this.context.csrf },
      body: JSON.stringify(body),
    })
    if (response.status === 401)
      this.context.authenticated = false
    if (!response.ok)
      throw new SignalingError(response.status)
    return response.status === 204 ? undefined : response.json()
  }

  start() {
    if (this.started || !this.context.ready || !this.context.authenticated || !this.context.installationId || typeof RTCPeerConnection === 'undefined')
      return
    this.started = true
    this.stopped = false
    this.blocked = false
    this.failures = 0
    window.addEventListener('online', this.networkChanged)
    window.addEventListener('offline', this.networkChanged)
    this.network = (navigator as Navigator & { connection?: NetworkInformation }).connection
    this.networkType = this.network?.type
    this.effectiveType = this.network?.effectiveType
    this.network?.addEventListener('change', this.networkEstimateChanged)
    document.addEventListener('visibilitychange', this.visibilityChanged)
    void this.connect(false)
  }

  private visibilityChanged = () => {
    clearTimeout(this.hidden)
    if (document.hidden) {
      this.hidden = setTimeout(() => this.fallback(true), 30000)
    }
    else if (!this.peer) {
      void this.connect(false)
    }
  }

  private networkChanged = () => {
    clearTimeout(this.networkTimer)
    this.networkType = this.network?.type
    this.effectiveType = this.network?.effectiveType
    this.blocked = false
    this.failures = 0
    this.fallback()
    if (navigator.onLine)
      void this.connect(true)
  }

  private networkEstimateChanged = () => {
    clearTimeout(this.networkTimer)
    if (this.network?.type !== this.networkType || this.network?.effectiveType !== this.effectiveType)
      this.networkTimer = setTimeout(this.networkChanged, 1000)
  }

  private route(route: TransportRoute) {
    if (this.context.transportRoute === route)
      return
    this.context.transportRoute = route
    window.dispatchEvent(new Event('leo-transport-change'))
  }

  private fallback(preserveRetry = false) {
    this.generation++
    this.controlAbort?.abort()
    clearTimeout(this.handshake)
    clearTimeout(this.heartbeat)
    this.traffic?.close()
    this.traffic = undefined
    this.route('relay')
    clearTimeout(this.timer)
    clearTimeout(this.expiry)
    if (!preserveRetry) {
      clearTimeout(this.retry)
      this.retry = undefined
      this.retryAt = 0
    }

    this.signals?.close()
    this.signals = undefined
    this.channel?.close()
    this.channel = undefined
    this.peer?.close()
    this.peer = undefined
    this.grant = undefined
  }

  private failed(error?: unknown) {
    if (error instanceof SignalingError && [401, 403].includes(error.status))
      this.blocked = true
    if (error instanceof DOMException && error.name === 'NotAllowedError')
      this.blocked = true
    this.fallback()
    if (!this.stopped && !this.blocked && navigator.onLine) {
      const delay = Math.min(300000, 30000 * 2 ** Math.min(this.failures++, 4))
      this.retryAt = Date.now() + delay
      this.retry = setTimeout(() => {
        this.retry = undefined
        void this.connect(false)
      }, delay)
    }
  }

  stop() {
    this.stopped = true
    this.started = false
    this.fallback()
    clearTimeout(this.hidden)
    clearTimeout(this.networkTimer)
    document.removeEventListener('visibilitychange', this.visibilityChanged)
    window.removeEventListener('online', this.networkChanged)
    window.removeEventListener('offline', this.networkChanged)
    this.network?.removeEventListener('change', this.networkEstimateChanged)
    this.network = undefined
  }

  private async connect(iceRestart: boolean) {
    if (this.stopped || this.blocked || this.online === false || !this.context.authenticated || !navigator.onLine || document.hidden)
      return
    // Visibility resumes an expired retry, but never shortens its deadline.
    if (Date.now() < this.retryAt)
      return
    this.fallback()
    this.controlAbort = new AbortController()
    const generation = this.generation
    const current = () => !this.stopped && this.generation === generation
    try {
      const peer = new RTCPeerConnection()
      this.peer = peer
      const channel = peer.createDataChannel('leo.v4', { ordered: true })
      this.channel = channel
      channel.binaryType = 'arraybuffer'
      channel.addEventListener('open', () => {
        if (current()) {
          this.failures = 0
          this.traffic = new DirectChannel(channel, this.grant!.grant.claims, () => {
            if (current())
              this.failed()
          })
          clearTimeout(this.handshake)
          this.route('direct')
          this.scheduleHeartbeat(current)
        }
      })
      channel.addEventListener('close', () => {
        if (current())
          this.failed()
      })
      channel.addEventListener('error', () => {
        if (current())
          this.failed()
      })
      peer.onconnectionstatechange = () => {
        if (current() && ['failed', 'closed'].includes(peer.connectionState))
          this.failed()
      }

      this.handshake = setTimeout(() => {
        if (current())
          this.failed()
      }, 30000)
      const offer = await peer.createOffer({ iceRestart })
      const fingerprint = /^a=fingerprint:(sha-256 [A-Fa-f0-9:]+)\r?$/m.exec(offer.sdp || '')?.[1]?.toUpperCase().replace('SHA-256', 'sha-256')
      if (!fingerprint)
        throw new Error('Missing DTLS fingerprint')
      const authorization = { fingerprint, versions: [4] }
      const grant = await this.control('/authorize', authorization) as GrantResponse
      if (!current())
        return
      if (!grant.available) {
        this.blocked = true
        this.fallback()
        return
      }

      this.grant = grant
      peer.setConfiguration({ iceServers: grant.iceServers })
      const path = `/${encodeURIComponent(grant.grant.claims.connection_id)}`
      const source = new EventSource(`${this.base}${path}/events`)
      this.signals = source
      let remoteReady = false
      const candidates: RTCIceCandidateInit[] = []
      let incoming = Promise.resolve()
      source.addEventListener('signal', (event) => {
        incoming = incoming.then(async () => {
          if (!current())
            return
          const signal = JSON.parse((event as MessageEvent).data)
          if (signal.kind === 'answer') {
            await peer.setRemoteDescription({ type: 'answer', sdp: usableSdp(signal.sdp) })
            remoteReady = true
            for (const candidate of candidates.splice(0))
              await peer.addIceCandidate(candidate)
          }
          else if (signal.kind === 'candidate' && signal.candidate && usableCandidate(signal.candidate)) {
            const candidate = { candidate: signal.candidate, sdpMid: signal.sdp_mid, sdpMLineIndex: signal.sdp_m_line_index }
            if (remoteReady)
              await peer.addIceCandidate(candidate)
            else if (candidates.length < 16)
              candidates.push(candidate)
            else
              throw new Error('Too many candidates')
          }
        }).catch(() => {
          if (current())
            this.failed()
        })
      })
      source.onerror = () => {
        if (current())
          this.failed()
      }

      let offerAccepted: () => void = () => {}
      let outgoing = new Promise<void>(resolve => offerAccepted = resolve)
      peer.onicecandidate = (event) => {
        const candidate = event.candidate
        if (candidate && !usableCandidate(candidate.candidate))
          return
        outgoing = outgoing.then(async () => {
          if (current()) {
            await this.control(`${path}/signal`, {
              kind: 'candidate',
              candidate: candidate?.candidate || '',
              sdp_mid: candidate?.sdpMid ?? '0',
              sdp_m_line_index: candidate?.sdpMLineIndex ?? 0,
            })
          }
        }).catch((error) => {
          // A single refused candidate does not invalidate usable ICE paths.
          const refusedCandidate = error instanceof SignalingError && [400, 429].includes(error.status)
          if (current() && !refusedCandidate)
            this.failed(error)
        })
      }

      // Send the offer before trickled candidates, using the certificate above.
      const numericOffer = usableSdp(offer.sdp || '')
      const sdp = numericOffer.replace(/a=fingerprint:sha-256 (.*)/g, (_, digest: string) => `a=fingerprint:sha-256 ${digest.toUpperCase()}`)
      await peer.setLocalDescription({ type: 'offer', sdp })
      if (!current())
        return
      await this.control(`${path}/signal`, { kind: 'offer', sdp })
      offerAccepted()
      if (!current())
        return
      this.scheduleRenewal(authorization, path, current)
    }
    catch (error) {
      if (current())
        this.failed(error)
    }
  }

  private async sendRequest(path: string, url: string, options: RequestInit = {}) {
    const traffic = this.context.transportRoute === 'direct' && (options.body == null || typeof options.body === 'string') ? this.traffic : undefined
    if (traffic) {
      try {
        return await traffic.request(`/api${path}`, options)
      }
      catch (error) {
        const method = (options.method || 'GET').toUpperCase()
        const message = method === 'POST' && /^\/chats\/[^/]+\/messages$/.test(path) && typeof options.body === 'string' && typeof JSON.parse(options.body).id === 'string'
        const replayable = error instanceof TransportNotSent || (error instanceof TransportLost && (method === 'GET' || method === 'HEAD' || message))
        if (!replayable)
          throw error
        const response = await fetch(url, options)
        if (message && response.status === 409) {
          const data = await response.clone().json()
          if (data.error === 'This message identifier has already been used.')
            return Response.json({}, { headers: response.headers })
        }

        return response
      }
    }

    return fetch(url, options)
  }

  async request(path: string, url: string, options: RequestInit = {}) {
    const response = await this.sendRequest(path, url, options)
    if (response.ok) {
      const route = response.headers.get('x-leo-transport')
      if (route === 'direct' || route === 'relay')
        observeTransport(route, path, options.method || 'GET')
    }

    return response
  }

  private scheduleHeartbeat(current: () => boolean) {
    this.heartbeat = setTimeout(async () => {
      const traffic = this.traffic
      if (!current() || !traffic)
        return
      const abort = new AbortController()
      let deadline: ReturnType<typeof setTimeout> | undefined
      try {
        await traffic.request('/api/chats', { method: 'HEAD', signal: abort.signal }, () => {
          deadline = setTimeout(() => abort.abort(), 5000)
        })
        if (current()) {
          this.failures = 0
          this.scheduleHeartbeat(current)
        }
      }
      catch (error) {
        if (current()) {
          if (error instanceof TransportNotSent)
            this.scheduleHeartbeat(current)
          else
            this.failed(error)
        }
      }
      finally { clearTimeout(deadline) }
    }, 10000)
  }

  source(path: string, url: string): EventSource {
    const traffic = this.context.transportRoute === 'direct' ? this.traffic : undefined
    if (!traffic)
      return Object.assign(new EventSource(url), { transportRoute: 'relay' as const })
    return new DirectSource(
      signal => traffic.request(`/api${path}`, { signal, headers: { accept: 'text/event-stream' } }),
      () => new EventSource(url),
    ) as unknown as EventSource
  }

  private scheduleRenewal(authorization: unknown, path: string, current: () => boolean, extended = true) {
    const remaining = this.grant!.grant.claims.expires_at * 1000 - Date.now()
    this.expiry = setTimeout(() => {
      if (current())
        this.failed()
    }, Math.max(0, remaining))
    // A session-capped grant cannot be extended. Keep its strict expiry, without
    // spending authorization quota on repeated renewals of the same deadline.
    if (!extended)
      return
    this.timer = setTimeout(async () => {
      try {
        const grant = await this.control(`${path}/renew`, authorization) as GrantResponse
        if (!current())
          return
        if (!grant.available) {
          this.blocked = true
          this.fallback()
          return
        }

        const previousExpiry = this.grant!.grant.claims.expires_at
        this.grant = grant
        clearTimeout(this.expiry)
        this.scheduleRenewal(authorization, path, current, grant.grant.claims.expires_at > previousExpiry)
      }
      catch (error) {
        if (current())
          this.failed(error)
      }
    }, Math.max(10000, remaining - 30000))
  }
}
