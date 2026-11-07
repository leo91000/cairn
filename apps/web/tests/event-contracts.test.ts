import type { RunEvent } from '../../../packages/contracts/generated/typescript/RunEvent'
import Ajv from 'ajv'
import { expect, it } from 'vitest'
import historySchema from '../../../packages/contracts/generated/HistoryPage.json'
import eventSchema from '../../../packages/contracts/generated/RunEvent.json'
import windowSchema from '../../../packages/contracts/generated/RunHistoryWindowOutput.json'
import corpus from '../../../tests/fixtures/event-contracts.json'

it('qualifies generated durable events, unknown engine frames and history cursors', () => {
  const ajv = new Ajv({ strict: false })
  const event = ajv.compile(eventSchema)
  const history = ajv.compile(historySchema)
  const window = ajv.compile(windowSchema)
  const values: RunEvent[] = corpus.events
  for (const value of values)
    expect(event(value), JSON.stringify(event.errors)).toBe(true)
  for (const value of corpus.invalidEvents)
    expect(event(value)).toBe(false)
  expect(window(corpus.windowOutput), JSON.stringify(window.errors)).toBe(true)
  expect(history(corpus.history), JSON.stringify(history.errors)).toBe(true)
})

it('uses the generated GET client with escaped history cursors', async () => {
  const { readRunHistory } = await import('../../../packages/contracts/generated/readRunHistory')
  const calls: [string, RequestInit][] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return corpus.history as T
  }

  const history = `${corpus.window.history}&next=?`
  await readRunHistory(request, { ...corpus.window, history })
  const [path, options] = calls[0]!
  const url = new URL(path, 'https://fixture.test')
  expect(url.pathname).toBe(`/runs/${corpus.window.runId}/history`)
  expect(url.searchParams.get('before')).toBe('100')
  expect(url.searchParams.get('history')).toBe(history)
  expect(options).toEqual({ method: 'GET' })
})
