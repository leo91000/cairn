import type { Provider } from '../../../packages/contracts/accounts'

export type ResumeDiagnosticStage
  = | 'handler-entered'
    | 'guard-no-detail'
    | 'guard-busy'
    | 'run-state'
    | 'selection-start'
    | 'catalog-state'
    | 'selection-ready'
    | 'resume-attempt'
    | 'resume-returned'
    | 'pause-attempt'
    | 'pause-returned'
    | 'caught'

export type ResumeDiagnosticRunStatus = 'missing' | 'other' | 'queued' | 'running' | 'failed' | 'interrupted' | 'cancelled' | 'succeeded' | 'idle'

export interface ResumeDiagnosticRecord {
  stage: ResumeDiagnosticStage
  runStatus?: ResumeDiagnosticRunStatus
  resumeAvailable?: boolean
  provider?: Provider | 'other'
  catalogPresent?: boolean
}

type ResumeDiagnosticWindow = Window & {
  __cairnTestResumeDiagnostic?: (record: ResumeDiagnosticRecord) => void
}

const resumeDiagnosticEnabled = import.meta.env.VITE_CAIRN_TEST_RESUME_DIAGNOSTICS === 'true'
let resumeDiagnosticCount = 0

export function reportResumeDiagnostic(record: ResumeDiagnosticRecord) {
  if (!resumeDiagnosticEnabled || resumeDiagnosticCount >= 16)
    return
  resumeDiagnosticCount++
  try {
    (window as ResumeDiagnosticWindow).__cairnTestResumeDiagnostic?.(record)
  }
  catch {
    // The optional test probe must never affect the resume action.
  }
}

export function resumeDiagnosticRunStatus(status?: string): ResumeDiagnosticRunStatus {
  switch (status) {
    case 'queued':
    case 'running':
    case 'failed':
    case 'interrupted':
    case 'cancelled':
    case 'succeeded':
    case 'idle':
      return status
    default:
      return status ? 'other' : 'missing'
  }
}

export function resumeDiagnosticProvider(provider: string): Provider | 'other' {
  if (provider === 'codex' || provider === 'claude' || provider === 'opencode' || provider === 'antigravity')
    return provider
  return 'other'
}
