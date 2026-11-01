import { writeFileSync } from 'node:fs'

const phases = new Set([
  'fixture-start',
  'beacon-start',
  'browser-launch',
  'browser-page',
  'capture-transport',
  'route-setup',
  'beacon-health',
  'beacon-stun',
  'page-navigate',
  'login-email',
  'login-code',
  'login-submit',
  'claim-open',
  'installation-start',
  'installation-bootstrap',
  'route-bootstrap',
  'account-session',
  'conversation-open',
  'message-fill',
  'route-promote',
  'message-send',
  'message-delivery',
  'message-heading',
  'rust-read',
  'anonymous-rust-read',
  'anonymous-local-read',
  'owner-session',
  'scenario-verification',
  'report-write',
  'route-check',
  'logout-session',
  'logout',
  'logged-out-rust-read',
  'complete',
])
const cleanupPhases = new Set(['capture-transport', 'browser-close', 'fixture-close', 'complete'])
const lifecycleEvents = new Set(['browser-disconnected', 'context-closed', 'page-closed', 'page-crashed'])

// Persist before awaiting an operation: Playwright's global timeout can close
// every Page before the test's finally block gets an opportunity to collect it.
export function networkPhaseDiagnostics(output) {
  const started = performance.now()
  const entered = []
  let testPhase
  let cleanupPhase

  function persist(entry) {
    entered.push({ ...entry, elapsedMs: Math.round(performance.now() - started) })
    if (entered.length > 128)
      entered.shift()

    if (output) {
      const path = output.endsWith('.json') ? `${output.slice(0, -5)}.phases.json` : `${output}.phases.json`
      writeFileSync(path, `${JSON.stringify({ testPhase, cleanupPhase, entered })}\n`, { mode: 0o600 })
    }
  }

  function enter(phase, cleanup = false) {
    const allowed = cleanup ? cleanupPhases : phases
    if (!allowed.has(phase))
      throw new Error('Unknown network qualification phase')

    if (cleanup)
      cleanupPhase = phase
    else
      testPhase = phase

    persist({ phase, cleanup })
  }

  function event(event) {
    if (!lifecycleEvents.has(event))
      throw new Error('Unknown network qualification lifecycle event')

    persist({ event })
  }

  return { enter, cleanup: phase => enter(phase, true), event }
}
