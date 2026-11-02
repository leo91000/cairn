interface NetworkPhaseDiagnostics {
  enter: (phase: string) => void
  cleanup: (phase: string) => void
  event: (event: string) => void
  finish: () => void
}

export function networkPhaseDiagnostics(output?: string): NetworkPhaseDiagnostics

export function closeNetworkFixture(phases: NetworkPhaseDiagnostics, resources: {
  captureTransport: () => Promise<void>
  browser?: { close: () => Promise<void> }
  fixture: { close: () => Promise<void> }
}): Promise<void>
