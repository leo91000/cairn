import { DirectChannel, TransportLost } from './direct-channel'
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

// #117: no hostname resolution. Unusable mDNS candidates keep the relay available.
function mdnsCandidate(candidate: string) {
  return candidate.trim().split(/\s+/)[4]?.toLowerCase().endsWith('.local') || false
}

// One transport per document/current installation. Signaling always uses HTTPS.
export class InstallationTransport {
  private peer?: RTCPeerConnection
  private channel?: RTCDataChannel
  private signals?: EventSource
  private timer?: ReturnType<typeof setTimeout>
  private expiry?: ReturnType<typeof setTimeout>
  private retry?: ReturnType<typeof setTimeout>
  private generation = 0
  private started = false
  private stopped = true
  private grant?: GrantResponse
  private traffic?: DirectChannel
  private handshake?: ReturnType<typeof setTimeout>
  private heartbeat?: ReturnType<typeof setTimeout>
  private controlAbort?: AbortController

  constructor(private readonly context: Context) {}

  private get base() {
    return `/api/installations/${encodeURIComponent(this.context.installationId)}/direct`
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
      throw new Error('Direct signaling unavailable')
    return response.status === 204 ? undefined : response.json()
  }

  start() {
    if (this.started || !this.context.ready || !this.context.authenticated || !this.context.installationId || typeof RTCPeerConnection === 'undefined')
      return
    this.started = true
    this.stopped = false
    window.addEventListener('online', this.networkChanged)
    window.addEventListener('offline', this.networkChanged)
    const network = (navigator as Navigator & { connection?: EventTarget }).connection
    network?.addEventListener('change', this.networkChanged)
    void this.connect(false)
  }

  private networkChanged = () => {
    this.fallback()
    if (navigator.onLine)
      void this.connect(true)
  }

  private route(route: TransportRoute) {
    if (this.context.transportRoute === route)
      return
    this.context.transportRoute = route
    window.dispatchEvent(new Event('leo-transport-change'))
  }

  private fallback() {
    this.generation++
    this.controlAbort?.abort()
    clearTimeout(this.handshake)
    clearTimeout(this.heartbeat)
    this.traffic?.close()
    this.traffic = undefined
    this.route('relay')
    clearTimeout(this.timer)
    clearTimeout(this.expiry)
    clearTimeout(this.retry)
    this.signals?.close()
    this.signals = undefined
    this.channel?.close()
    this.channel = undefined
    this.peer?.close()
    this.peer = undefined
    this.grant = undefined
  }

  private failed() {
    this.fallback()
    if (!this.stopped && navigator.onLine)
      this.retry = setTimeout(() => void this.connect(false), 30000)
  }

  stop() {
    this.stopped = true
    this.started = false
    this.fallback()
    window.removeEventListener('online', this.networkChanged)
    window.removeEventListener('offline', this.networkChanged)
    const network = (navigator as Navigator & { connection?: EventTarget }).connection
    network?.removeEventListener('change', this.networkChanged)
  }

  private async connect(iceRestart: boolean) {
    if (this.stopped || !navigator.onLine)
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
        this.failed()
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
            await peer.setRemoteDescription({ type: 'answer', sdp: signal.sdp })
            remoteReady = true
            for (const candidate of candidates.splice(0))
              await peer.addIceCandidate(candidate)
          }
          else if (signal.kind === 'candidate' && signal.candidate && !mdnsCandidate(signal.candidate)) {
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
        if (candidate && mdnsCandidate(candidate.candidate))
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
        }).catch(() => {
          if (current())
            this.failed()
        })
      }

      // Send the offer before trickled candidates, using the certificate above.
      const numericOffer = (offer.sdp || '').split('\r\n').filter(line => !line.startsWith('a=candidate:') || !mdnsCandidate(line.slice(2))).join('\r\n')
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
    catch {
      if (current())
        this.failed()
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
        if (!(error instanceof TransportLost) || (method !== 'GET' && method !== 'HEAD' && !message))
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
      const deadline = setTimeout(() => abort.abort(), 5000)
      try {
        await traffic.request('/api/chats', { method: 'HEAD', signal: abort.signal })
        if (current())
          this.scheduleHeartbeat(current)
      }
      catch {
        if (current())
          this.failed()
      }
      finally { clearTimeout(deadline) }
    }, 10000)
  }

  source(path: string, url: string): EventSource {
    const traffic = this.context.transportRoute === 'direct' ? this.traffic : undefined
    if (!traffic)
      return Object.assign(new EventSource(url), { transportRoute: 'relay' as const })
    return new DirectSource(signal => traffic.request(`/api${path}`, { signal, headers: { accept: 'text/event-stream' } })) as unknown as EventSource
  }

  private scheduleRenewal(authorization: unknown, path: string, current: () => boolean) {
    const remaining = this.grant!.grant.claims.expires_at * 1000 - Date.now()
    this.expiry = setTimeout(() => {
      if (current())
        this.failed()
    }, Math.max(0, remaining))
    this.timer = setTimeout(async () => {
      try {
        const grant = await this.control(`${path}/renew`, authorization) as GrantResponse
        if (!current())
          return
        if (!grant.available)
          throw new Error('Renewal unavailable')
        this.grant = grant
        clearTimeout(this.expiry)
        this.scheduleRenewal(authorization, path, current)
      }
      catch {
        if (current())
          this.failed()
      }
    }, Math.max(1000, remaining - 30000))
  }
}
