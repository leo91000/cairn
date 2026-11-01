export const RESUME_DIAGNOSTIC_MARKER = 'CAIRN_RESUME_DIAGNOSTIC'

const MAX_RECORDS = 16
const MAX_REQUEST_COUNT = 10
const stages = new Set([
  'handler-entered',
  'guard-no-detail',
  'guard-busy',
  'run-state',
  'selection-start',
  'catalog-state',
  'selection-ready',
  'resume-attempt',
  'resume-returned',
  'pause-attempt',
  'pause-returned',
  'caught',
])
const runStatuses = new Set([
  'missing',
  'other',
  'queued',
  'running',
  'failed',
  'interrupted',
  'cancelled',
  'succeeded',
  'idle',
])
const providers = new Set(['codex', 'claude', 'opencode', 'other'])

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

export type ResumeDiagnosticRunStatus
  = | 'missing'
    | 'other'
    | 'queued'
    | 'running'
    | 'failed'
    | 'interrupted'
    | 'cancelled'
    | 'succeeded'
    | 'idle'

export type ResumeDiagnosticProvider = 'codex' | 'claude' | 'opencode' | 'other'

export interface ResumeDiagnosticRecord {
  stage: ResumeDiagnosticStage
  runStatus?: ResumeDiagnosticRunStatus
  resumeAvailable?: boolean
  provider?: ResumeDiagnosticProvider
  catalogPresent?: boolean
}

export interface ResumeDiagnosticReport {
  marker: typeof RESUME_DIAGNOSTIC_MARKER
  available: boolean
  requestCount: number
  recordsTruncated: boolean
  stages: ResumeDiagnosticRecord[]
}

function allowlistedString(value: unknown, allowed: Set<string>) {
  return typeof value === 'string' && allowed.has(value) ? value : undefined
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

export function projectResumeDiagnostic(
  requestCount: number,
  source: unknown,
): ResumeDiagnosticReport {
  const boundedRequestCount = Number.isFinite(requestCount)
    ? Math.min(MAX_REQUEST_COUNT, Math.max(0, Math.trunc(requestCount)))
    : 0
  if (!Array.isArray(source)) {
    return {
      marker: RESUME_DIAGNOSTIC_MARKER,
      available: false,
      requestCount: boundedRequestCount,
      recordsTruncated: false,
      stages: [],
    }
  }

  const projected: ResumeDiagnosticRecord[] = []
  for (const candidate of source.slice(0, MAX_RECORDS)) {
    if (!isRecord(candidate))
      continue
    const stage = allowlistedString(candidate.stage, stages)
    if (!stage)
      continue
    const record: ResumeDiagnosticRecord = { stage: stage as ResumeDiagnosticStage }
    const runStatus = allowlistedString(candidate.runStatus, runStatuses)
    if (runStatus)
      record.runStatus = runStatus as ResumeDiagnosticRunStatus
    const provider = allowlistedString(candidate.provider, providers)
    if (provider)
      record.provider = provider as ResumeDiagnosticProvider
    if (typeof candidate.resumeAvailable === 'boolean')
      record.resumeAvailable = candidate.resumeAvailable
    if (typeof candidate.catalogPresent === 'boolean')
      record.catalogPresent = candidate.catalogPresent
    projected.push(record)
  }

  return {
    marker: RESUME_DIAGNOSTIC_MARKER,
    available: true,
    requestCount: boundedRequestCount,
    recordsTruncated: source.length > MAX_RECORDS,
    stages: projected,
  }
}
