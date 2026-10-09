import { describe, expect, it } from 'vitest'
import { contentParts, dataTitle, statusTone } from '../src/activity-data'

describe('structured activity content', () => {
  it('recognizes the workflow array and prefixed pull request output from historical messages', () => {
    const runs = [{
      id: 34375012123,
      head: 'f467d688',
      status: 'completed',
      conclusion: 'success',
      jobs: [{ name: 'native-quality', conclusion: 'success' }, { name: 'changeset-policy', conclusion: 'skipped' }],
    }]
    const parts = contentParts(JSON.stringify(runs))
    expect(parts).toEqual([{ kind: 'data', value: runs, source: JSON.stringify(runs) }])
    expect(dataTitle(runs)).toBe('Workflow checks')
    const pr = { baseRefOid: 'bde80b78', body: '', files: ['.changeset/baseline.md', 'crates/sheetom-core/src/lib.rs'] }
    expect(contentParts(`implementation-pr ${JSON.stringify(pr)}\nChecks are complete.`)).toEqual([
      { kind: 'text', text: 'implementation-pr ' },
      { kind: 'data', value: pr, source: JSON.stringify(pr) },
      { kind: 'text', text: '\nChecks are complete.' },
    ])
    expect(dataTitle(pr)).toBe('Pull request details')
  })
  it('handles braces in strings, multiple results and JSON code fences while preserving other Markdown', () => {
    const source = 'First {"message":"escaped \\\" } [","count":2}\nThen [{"ok":true}]'
    expect(contentParts(source).map(part => part.kind)).toEqual(['text', 'data', 'text', 'data'])
    expect(contentParts('```json\n{"files":[]}\n```')[0]).toMatchObject({ kind: 'data', value: { files: [] } })
    for (const text of ['Use `{ "x": 1 }` here.', '```js\nconst data = {"x":1}\n```', '[1](https://example.com)', 'Ordinary {prose} and [labels].'])
      expect(contentParts(text)).toEqual([{ kind: 'text', text }])
  })
  it('preserves malformed data instead of inventing a valid result', () => {
    for (const text of ['{"x": nope}', '{"x": [1,2}'])
      expect(contentParts(text)).toEqual([{ kind: 'text', text }])
  })
  it('identifies incomplete saved JSON without pretending nested fragments are complete results', () => {
    for (const source of ['[{"jobs":[{"name":"test"}]}', '{"outer":{"complete":true},"unfinished":'])
      expect(contentParts(source)).toEqual([{ kind: 'incomplete', source }])
    expect(contentParts('implementation-pr {"files":["src/a.ts"')).toEqual([
      { kind: 'text', text: 'implementation-pr ' },
      { kind: 'incomplete', source: '{"files":["src/a.ts"' },
    ])
    expect(contentParts('```json\n{"x":')).toEqual([{ kind: 'incomplete', source: '{"x":' }])
    expect(contentParts('```json\n{"x":\n```')).toEqual([{ kind: 'incomplete', source: '{"x":\n' }])
  })
  it('only adds semantic colors to explicit status fields', () => {
    expect(statusTone('conclusion', 'failure')).toBe('error')
    expect(statusTone('conclusion', 'success')).toBe('success')
    expect(statusTone('conclusion', 'skipped')).toBe('neutral')
    expect(statusTone('status', 'completed')).toBe('neutral')
    expect(statusTone('message', 'success')).toBe('')
  })
})
