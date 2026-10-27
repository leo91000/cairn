import type { RunEvent } from '../../../packages/contracts/generated/typescript/RunEvent'
import Ajv from 'ajv'
import { expect, it } from 'vitest'
import historySchema from '../../../packages/contracts/generated/HistoryPage.json'
import eventSchema from '../../../packages/contracts/generated/RunEvent.json'
import corpus from '../../../tests/fixtures/event-contracts.json'

it('qualifies generated durable events, unknown engine frames and history cursors', () => {
  const ajv = new Ajv({ strict: false })
  const event = ajv.compile(eventSchema)
  const history = ajv.compile(historySchema)
  const values: RunEvent[] = corpus.events
  for (const value of values)
    expect(event(value), JSON.stringify(event.errors)).toBe(true)
  for (const value of corpus.invalidEvents)
    expect(event(value)).toBe(false)
  expect(history(corpus.history), JSON.stringify(history.errors)).toBe(true)
})
