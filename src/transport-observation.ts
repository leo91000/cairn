import type { TransportRoute } from './installation-transport'

// Only successful application traffic is evidence; signaling/ICE state is not.
export function observeTransport(route: TransportRoute, path: string, method: string, cursor?: number) {
  window.dispatchEvent(new CustomEvent('leo-transport-observation', {
    detail: {
      route,
      path,
      method,
      cursor,
    },
  }))
}
