import type { RunEvent } from '../../../packages/contracts/contracts'
import type { TimelineEntry } from '../../../packages/contracts/live'
import { expect, it } from 'vitest'
import { LiveTimeline } from '../src/live-timeline'

function event(id: number): RunEvent {
  return {
    id,
    runId: 'r',
    createdAt: id,
    type: 'turn.started',
    text: '',
    timeline: true,
  }
}

function reply(position: number, revision: number, text: string): TimelineEntry {
  return {
    id: `message:1:${position}`,
    runId: 'r',
    position,
    revision,
    source: revision,
    turn: 1,
    attempt: 0,
    createdAt: position,
    updatedAt: revision,
    content: {
      kind: 'agentMessage',
      itemId: `${position}`,
      text,
      state: 'running',
    },
  }
}

it('applies batches, duplicates and reconnects without repeating events or regressing cards', () => {
  const live = new LiveTimeline()
  live.apply([event(1), event(2)], [{ kind: 'upsert', entry: reply(3, 3, 'Hello') }], 3)
  live.apply([event(2), event(4)], [{
    kind: 'append',
    id: 'message:1:3',
    revision: 5,
    source: 5,
    updatedAt: 5,
    text: ' world',
  }], 5)
  // A history page read earlier carries an older revision of the same card.
  live.merge([event(1)], [reply(3, 3, 'Hello')])
  expect(live.events.map(item => item.id)).toEqual([1, 2, 4])
  expect(live.cursor).toBe(5)
  expect(live.entries()).toMatchObject([{ revision: 5, content: { text: 'Hello world' } }])
  live.apply([], [{
    kind: 'upsert',
    entry: {
      ...reply(3, 7, 'Hello world!'),
      content: {
        kind: 'agentMessage',
        itemId: '3',
        text: 'Hello world!',
        state: 'done',
      },
    },
  }], 7)
  expect(live.entries()[0]!.content).toMatchObject({ text: 'Hello world!', state: 'done' })
})

it('refuses appended text without the baseline this stream delivered', () => {
  const live = new LiveTimeline()
  expect(() => live.apply([], [{
    kind: 'append',
    id: 'missing',
    revision: 2,
    source: 2,
    updatedAt: 2,
    text: 'x',
  }])).toThrow('Missing streamed message baseline')
})

it('keeps the stream baseline separate from a newer overlapping history page', () => {
  const live = new LiveTimeline()
  live.apply([], [{ kind: 'upsert', entry: reply(3, 3, 'A') }], 3)
  live.merge([], [reply(3, 5, 'ABC')])
  live.apply([], [{
    kind: 'append',
    id: 'message:1:3',
    revision: 4,
    source: 4,
    updatedAt: 4,
    text: 'B',
  }], 4)
  expect(live.entries()).toMatchObject([{ revision: 5, content: { text: 'ABC' } }])
  live.apply([], [{
    kind: 'append',
    id: 'message:1:3',
    revision: 6,
    source: 6,
    updatedAt: 6,
    text: 'CD',
  }], 6)
  expect(live.entries()).toMatchObject([{ revision: 6, content: { text: 'ABCD' } }])
})

it('orders cards by first appearance, whatever page delivered them', () => {
  const live = new LiveTimeline()
  live.apply([], [{ kind: 'upsert', entry: reply(20, 20, 'Later') }], 20)
  live.merge([], [reply(5, 9, 'Earlier')])
  expect(live.entries().map(card => card.position)).toEqual([5, 20])
})
