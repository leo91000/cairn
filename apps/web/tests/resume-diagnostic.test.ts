import { describe, expect, it } from 'vitest'
import { projectResumeDiagnostic } from '../../../tests/e2e/resume-diagnostic'

describe('resume failure diagnostics', () => {
  it('keeps only fixed stages, enum values, booleans, and a capped count', () => {
    const report = projectResumeDiagnostic(123, [{
      stage: 'run-state',
      runStatus: 'failed',
      resumeAvailable: true,
      provider: 'codex',
      catalogPresent: false,
      chatId: 'private-chat-id',
      model: 'private-model-name',
      error: 'private error text',
      payload: { token: 'private-payload' },
    }, {
      stage: 'private-stage',
      runStatus: 'private-status',
      provider: 'private-provider',
      resumeAvailable: 'true',
      catalogPresent: 1,
      chatId: 'private-chat-id',
    }])

    expect(report).toEqual({
      marker: 'CAIRN_RESUME_DIAGNOSTIC',
      available: true,
      requestCount: 10,
      recordsTruncated: false,
      stages: [{
        stage: 'run-state',
        runStatus: 'failed',
        resumeAvailable: true,
        provider: 'codex',
        catalogPresent: false,
      }],
    })
    const serialized = JSON.stringify(report)
    for (const privateValue of ['private-chat-id', 'private-model-name', 'private error text', 'private-payload', 'private-stage', 'private-status', 'private-provider'])
      expect(serialized).not.toContain(privateValue)
  })

  it('caps stages and marks truncated records', () => {
    const report = projectResumeDiagnostic(2, Array.from({ length: 17 }, () => ({ stage: 'resume-attempt' })))

    expect(report.requestCount).toBe(2)
    expect(report.recordsTruncated).toBe(true)
    expect(report.stages).toHaveLength(16)
    expect(report.stages.every(stage => stage.stage === 'resume-attempt')).toBe(true)
  })

  it('uses a fixed unavailable report for missing or malformed records', () => {
    expect(projectResumeDiagnostic(1, undefined)).toEqual({
      marker: 'CAIRN_RESUME_DIAGNOSTIC',
      available: false,
      requestCount: 1,
      recordsTruncated: false,
      stages: [],
    })
    expect(projectResumeDiagnostic(Number.POSITIVE_INFINITY, 'not records').requestCount).toBe(0)
  })
})
