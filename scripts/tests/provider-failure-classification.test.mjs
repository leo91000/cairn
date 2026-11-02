import { describe, expect, it } from 'vitest'
import { classifyFailure } from '../provider-failure-classification.mjs'

describe('health-watch provider failure classification', () => {
  it('recognizes the exact provider model-capacity message', () => {
    expect(classifyFailure('Selected model is at capacity. Please try a different model.'))
      .toBe('provider_capacity')
  })

  it('recognizes the structured Codex overload code without serializing the error', () => {
    const error = { codexErrorInfo: 'serverOverloaded', message: 'private detail' }

    expect(classifyFailure(error)).toBe('provider_capacity')
    expect(JSON.stringify(classifyFailure(error))).not.toContain('private detail')
  })

  it('does not treat general node or disk capacity as provider model capacity', () => {
    expect(classifyFailure('Node capacity is exhausted')).toBe('other')
    expect(classifyFailure('Disk capacity is low')).toBe('storage')
    expect(classifyFailure({ code: 'serverOverloaded' })).toBe('other')
  })

  it('preserves existing class labels and precedence', () => {
    expect(classifyFailure('Workspace timed out')).toBe('workspace_timeout')
    expect(classifyFailure('SQLite disk error')).toBe('storage')
    expect(classifyFailure('Request timed out')).toBe('timeout')
    expect(classifyFailure('HTTP 401 unauthorized')).toBe('other')
    expect(classifyFailure('usage limit reached')).toBe('account')
    expect(classifyFailure('unrecognized provider response')).toBe('other')
  })
})
