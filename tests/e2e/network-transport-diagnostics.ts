import type { Page } from '@playwright/test'

// Keep only transport state and counters: never SDP, candidates, URLs or payloads.
export async function captureNetworkTransport(target: Page) {
  await target.addInitScript(() => {
    const browser = window as typeof window & { networkTransport: { kind: string, elapsedMs: number }[] }
    browser.networkTransport = []
    let peers = 0

    function record(kind: string, values: Record<string, unknown>) {
      browser.networkTransport.push({ elapsedMs: performance.now(), kind, ...values })
      if (browser.networkTransport.length > 256)
        browser.networkTransport.shift()
    }

    browser.addEventListener('cairn-direct-heartbeat-timeout', () => record('heartbeat-timeout', {}))
    browser.RTCPeerConnection = new Proxy(browser.RTCPeerConnection, {
      construct(target, args) {
        const peer = Reflect.construct(target, args) as RTCPeerConnection
        const index = ++peers

        record('created', { peer: index })
        for (const event of ['iceconnectionstatechange', 'connectionstatechange', 'icegatheringstatechange', 'signalingstatechange']) {
          peer.addEventListener(event, () => record(event, {
            peer: index,
            ice: peer.iceConnectionState,
            connection: peer.connectionState,
            gathering: peer.iceGatheringState,
            signaling: peer.signalingState,
          }))
        }

        const createChannel = peer.createDataChannel.bind(peer)
        peer.createDataChannel = (...args) => {
          const channel = createChannel(...args)
          for (const event of ['open', 'close', 'error'])
            channel.addEventListener(event, () => record(`channel-${event}`, { peer: index, state: channel.readyState }))
          return channel
        }

        const timer = setInterval(async () => {
          if (peer.connectionState === 'closed') {
            clearInterval(timer)
            return
          }

          const stats = await peer.getStats().catch(() => undefined)
          stats?.forEach((item) => {
            if (item.type === 'candidate-pair' && (item.nominated || item.state === 'in-progress')) {
              record('candidate-pair', {
                peer: index,
                state: item.state,
                nominated: item.nominated,
                requestsSent: item.requestsSent,
                responsesReceived: item.responsesReceived,
                bytesSent: item.bytesSent,
                bytesReceived: item.bytesReceived,
              })
            }

            if (item.type === 'transport') {
              record('transport', {
                peer: index,
                dtlsState: item.dtlsState,
                iceState: item.iceState,
                packetsSent: item.packetsSent,
                packetsReceived: item.packetsReceived,
              })
            }

            if (item.type === 'data-channel') {
              record('data-channel', {
                peer: index,
                state: item.state,
                messagesSent: item.messagesSent,
                messagesReceived: item.messagesReceived,
                bytesSent: item.bytesSent,
                bytesReceived: item.bytesReceived,
              })
            }
          })
        }, 1000)
        return peer
      },
    })
  })
}

export async function networkTransportDiagnostics(target: Page) {
  return target.evaluate(() => (window as typeof window & { networkTransport: { kind: string, elapsedMs: number }[] }).networkTransport)
}
