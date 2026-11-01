export function networkPhaseDiagnostics(output?: string): {
  enter: (phase: string) => void
  cleanup: (phase: string) => void
  event: (event: string) => void
}
