import type { RunEvent } from '../shared/contracts'
import { expect, it } from 'vitest'
import { activityEntries } from '../src/activity'
import { LiveEvents } from '../src/live-events'

it('preserves messages and tool order across batches, duplicates, reconnect and reused item ids', () => {
  const event = (id: number, type: string, text = ''): RunEvent => ({
    id,
    runId: 'r',
    createdAt: id,
    type,
    text,
    payload: type.startsWith('item.') ? { item: { id: 'same', type: 'agent_message', text } } : undefined,
  })
  const input = [event(1, 'turn.started'), event(2, 'item.updated', 'Hello'), event(3, 'item.updated', 'Hello world'), event(4, 'item.completed', 'Hello world!'), event(5, 'turn.started'), event(6, 'item.updated', 'Next'), event(7, 'item.completed', 'Next answer')]
  const reducer = new LiveEvents()
  const actual: RunEvent[] = []
  reducer.append(actual, input.slice(0, 3))
  reducer.append(actual, input.slice(1, 5))
  reducer.append(actual, input.slice(4))
  expect(reducer.cursor).toBe(7)
  expect(actual).toHaveLength(4)
  expect(activityEntries(actual)).toEqual(activityEntries(input))
})

it('bounds a long streaming message to one current snapshot', () => {
  const events: RunEvent[] = []
  const reducer = new LiveEvents()
  for (let id = 1; id <= 2000; id++) {
    reducer.append(events, [{
      id,
      runId: 'r',
      type: 'item.updated',
      createdAt: id,
      text: `Part ${id}`,
      payload: { item: { id: 'm', type: 'agent_message', text: `Part ${id}` } },
    }])
  }

  expect(events).toHaveLength(1)
  expect(events[0].text).toBe('Part 2000')
  expect(reducer.cursor).toBe(2000)
})

it('reconstructs Unicode deltas, ignores repeated batches, and accepts a new baseline on reconnect', () => {
  const reducer = new LiveEvents()
  const target: RunEvent[] = []
  const event = (id: number, item: object): RunEvent => ({
    id,
    runId: 'r',
    type: 'item.updated',
    createdAt: id,
    text: '',
    payload: { item: { id: 'm', type: 'agent_message', ...item } },
  })
  const first = event(1, { text: 'Bonjour 👋' })
  const delta = event(2, { delta: ' café' })
  reducer.append(target, [first, delta])
  reducer.append(target, [delta])
  expect(target[0].text).toBe('Bonjour 👋 café')
  reducer.append(target, [event(3, { text: 'Bonjour 👋 café !' }), event(4, { delta: ' Fin.' })])
  expect(target).toHaveLength(1)
  expect(target[0].text).toBe('Bonjour 👋 café ! Fin.')
})

it('refuses a delta without a baseline without advancing the cursor', () => {
  const reducer = new LiveEvents()
  expect(() => reducer.append([], [{
    id: 2,
    runId: 'r',
    type: 'item.updated',
    createdAt: 1,
    text: '',
    payload: { item: { id: 'm', type: 'agent_message', delta: 'lost?' } },
  }])).toThrow('baseline')
  expect(reducer.cursor).toBe(0)
})

it('restores a folded cached baseline before accepting further deltas', () => {
  const target: RunEvent[] = [{
    id: 7,
    runId: 'r',
    createdAt: 2,
    type: 'item.updated',
    text: 'Bonjour',
    payload: { item: { id: 'm', type: 'agent_message', text: 'Bonjour' } },
  }]
  const reducer = new LiveEvents()
  reducer.restore(target, 7)
  reducer.append(target, [{
    id: 8,
    runId: 'r',
    createdAt: 8,
    type: 'item.updated',
    text: '',
    payload: { item: { id: 'm', type: 'agent_message', delta: ' Leo' } },
  }])
  expect(target).toHaveLength(1)
  expect(target[0].text).toBe('Bonjour Leo')
  expect(target[0].createdAt).toBe(2)
})
