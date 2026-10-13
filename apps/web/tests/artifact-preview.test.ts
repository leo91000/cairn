import { describe, expect, it } from 'vitest'
import { formatJson, inertDocument, parseDelimited } from '../src/artifact-preview'

describe('artifact previews', () => {
  it('parses quoted cells, escaped quotes, CRLF and multiline values', () => {
    const table = parseDelimited('﻿name,notes\r\n"Doe, Jane","said ""hi"""\r\nBob,"two\nlines"\n', ',')
    expect(table).toEqual({
      rows: [['name', 'notes'], ['Doe, Jane', 'said "hi"'], ['Bob', 'two\nlines']],
      columns: 2,
      truncated: false,
    })
  })

  it('keeps ragged tab separated rows and reports truncation', () => {
    expect(parseDelimited('a\tb\tc\n1\t2', '\t')).toEqual({ rows: [['a', 'b', 'c'], ['1', '2']], columns: 3, truncated: false })
    const large = parseDelimited(Array.from({ length: 5 }, (_, i) => `row ${i}`).join('\n'), ',', 3)
    expect(large.rows).toEqual([['row 0'], ['row 1'], ['row 2']])
    expect(large.truncated).toBe(true)
    expect(parseDelimited('a\nb\n', ',', 2).truncated).toBe(false)
    expect(parseDelimited('""', ',').rows).toEqual([['']])
    expect(parseDelimited('a\n""', ',').rows).toEqual([['a'], ['']])
    expect(parseDelimited('', ',').rows).toEqual([])
  })

  it('bounds wide ragged tables to 10,000 rendered cells', () => {
    const table = parseDelimited(`${','.repeat(4999)}\n${'x\n'.repeat(999)}`, ',')
    expect(table.rows).toHaveLength(200)
    expect(table.columns).toBe(50)
    expect(table.rows.every(row => row.length <= 50)).toBe(true)
    expect(table.truncated).toBe(true)
    expect(parseDelimited('a,'.repeat(50), ',').truncated).toBe(true)
  })

  it('formats JSON without changing large numbers or duplicate keys', () => {
    expect(formatJson('{"id":9007199254740993,"id":1e400,"nested":[],"object":{}}')).toBe([
      '{',
      '  "id": 9007199254740993,',
      '  "id": 1e400,',
      '  "nested": [],',
      '  "object": {}',
      '}',
    ].join('\n'))
    expect(formatJson('{invalid')).toBe('{invalid')
    const deeplyNested = `${'['.repeat(101)}0${']'.repeat(101)}`
    expect(formatJson(deeplyNested)).toBe(deeplyNested)
    expect(formatJson('{"escaped":"a\\\\b\\\"c","n":-0}')).toContain('"n": -0')
  })

  it('falls back to the source before deeply indented JSON expands too far', () => {
    const nested = `${'['.repeat(99)}${Array.from({ length: 250000 }).fill('0').join(',')}${']'.repeat(99)}`
    expect(formatJson(nested)).toBe(nested)
  })

  it('places an offline content policy before the document content', () => {
    expect(inertDocument('<!DOCTYPE html><html><body>Report</body></html>')).toMatch(/^<!DOCTYPE html><meta http-equiv="Content-Security-Policy" content="default-src 'none';[^"]*"><html>/)
    expect(inertDocument('<p>Fragment</p>')).toMatch(/^<meta http-equiv="Content-Security-Policy"[^>]*><p>Fragment<\/p>$/)
  })
})
