import { describe, expect, it } from 'vitest'
import { classifyFailure } from '../provider-failure-classification.mjs'

describe('health-watch provider failure classification', () => {
  it('recognizes the exact provider model-capacity message', () => {
    expect(classifyFailure('Selected model is at capacity. Please try a different model.'))
      .toBe('provider_capacity')
  })

  it('requires the model-capacity phrase to begin the error text', () => {
    expect(classifyFailure('Wrapper says: selected model is at capacity; retrying unrelated operation'))
      .toBe('other')
    expect(classifyFailure('  SELECTED\tMODEL is at capacity. private-context-marker'))
      .toBe('provider_capacity')
  })

  it('recognizes a standalone overload code and returns no raw error text', () => {
    const error = 'Selected model is at capacity. private-context-marker'
    const category = classifyFailure(error)

    expect(category).toBe('provider_capacity')
    expect(classifyFailure('serverOverloaded')).toBe('provider_capacity')
    expect(JSON.stringify(category)).not.toContain('private-context-marker')
  })

  it('does not infer provider capacity from conflicting object-shaped errors', () => {
    expect(classifyFailure({
      codexErrorInfo: 'serverOverloaded',
      httpStatusCode: 401,
      message: 'Selected model is at capacity. Unauthorized',
    })).toBe('other')
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
