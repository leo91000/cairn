import Ajv from 'ajv'
import { expect, it } from 'vitest'
import windowSchema from '../../../packages/contracts/generated/RunTimelineOutput.json'
import changeSchema from '../../../packages/contracts/generated/TimelineChange.json'
import entrySchema from '../../../packages/contracts/generated/TimelineEntry.json'
import corpus from '../../../tests/fixtures/timeline-contracts.json'
import served from '../../../tests/fixtures/timeline-corpus.json'

it('qualifies generated timeline cards, live changes and tool windows', () => {
  const ajv = new Ajv({ strict: false })
  const entry = ajv.compile(entrySchema)
  const change = ajv.compile(changeSchema)
  const window = ajv.compile(windowSchema)
  for (const value of corpus.entries)
    expect(entry(value), JSON.stringify(entry.errors)).toBe(true)
  for (const value of corpus.changes)
    expect(change(value), JSON.stringify(change.errors)).toBe(true)
  expect(window(corpus.window), JSON.stringify(window.errors)).toBe(true)
  for (const value of corpus.invalidEntries)
    expect(entry(value)).toBe(false)
  // Every card the server produced for the shared corpus satisfies the same schema.
  for (const run of served.runs) {
    for (const value of run.entries)
      expect(entry(value), JSON.stringify(entry.errors)).toBe(true)
  }
})
