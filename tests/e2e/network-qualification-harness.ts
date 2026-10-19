// This bundle is injected only into the qualification document. It imports the
// shipped transport and live consumer; authentication and dispatch stay real.
import { apiUrl, installationTransport, state } from '../../apps/web/src/api'
import { liveConnection } from '../../apps/web/src/live-connection'

const accepted: {
  cursor: number
  reset: boolean
  history?: string
  ids: number[]
  messages: string[]
}[] = []
let live: ReturnType<typeof liveConnection> | undefined
let visibleMessages: { id: string, text: string }[] = []
const requests: Record<string, {
  done: boolean
  status?: number
  route?: string | null
  failed?: boolean
}> = {}

const harness = {
  accepted,
  requests,
  configure(installationId: string, csrf: string) {
    Object.assign(state, {
      installationId,
      csrf,
      authenticated: true,
      ready: true,
    })
  },
  route: () => state.transportRoute,
  start: () => installationTransport.start(),
  stopNegotiation: () => installationTransport.stop(),
  stop() {
    live?.close()
    installationTransport.stop()
  },
  live(chat: string) {
    live?.close()
    live = liveConnection(`/chats/${chat}/stream`, (batch, cursor) => {
      if (batch.state?.chat)
        visibleMessages = batch.state.chat.messages
      const messages = visibleMessages
      accepted.push({
        cursor,
        reset: batch.reset,
        history: batch.history,
        ids: batch.events.map(event => event.id),
        messages: messages.map(message => message.id),
      })
      if (accepted.length > 128)
        throw new Error('qualification batch bound exceeded')
      document.getElementById('content')!.textContent = messages.map(message => message.text).join('\n')
    }, () => {})
  },
  request(key: string, path: string, method = 'GET', body?: unknown) {
    requests[key] = { done: false }
    void installationTransport.request(path, apiUrl(path), {
      method,
      headers: { 'content-type': 'application/json', 'x-csrf-token': state.csrf },
      body: body === undefined ? undefined : JSON.stringify(body),
    }).then(async (response) => {
      await response.arrayBuffer()
      requests[key] = { done: true, status: response.status, route: response.headers.get('x-cairn-transport') }
    }).catch(() => {
      requests[key] = { done: true, failed: true }
      const failure = document.createElement('output')
      failure.dataset.operation = key
      failure.textContent = 'Request failed'
      document.body.append(failure)
    })
  },
}

Object.assign(window, { ci4: harness })
