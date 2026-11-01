import type { TimelineEntry } from '../../../packages/contracts/live'
import {
  beforeEach,
  expect,
  it,
  vi,
} from 'vitest'
import { api } from '../src/api'
import { readLargeTimeline } from '../src/large-timeline'

vi.mock('../src/api', () => ({ api: vi.fn() }))

const card: TimelineEntry = {
  id: 'message:1:é',
  runId: 'run',
  position: 1,
  revision: 2,
  source: 2,
  turn: 1,
  attempt: 0,
  createdAt: 1,
  updatedAt: 2,
  content: {
    kind: 'agentMessage',
    itemId: 'é',
    text: '🦊 result',
    state: 'done',
  },
}
const preview = { snapshot: { ...card, content: { ...card.content, itemId: '', text: '' } } }

beforeEach(() => vi.mocked(api).mockReset())

it('reassembles UTF-8 chunks before returning a full card', async () => {
  const json = JSON.stringify(card)
  const split = json.indexOf('🦊')
  const first = json.slice(0, split)
  const second = json.slice(split)
  const encoder = new TextEncoder()
  const next = encoder.encode(first).length
  const total = encoder.encode(json).length
  vi.mocked(api)
    .mockResolvedValueOnce({
      entryId: card.id,
      runId: card.runId,
      offset: 0,
      totalBytes: total,
      sha256: 'a'.repeat(64),
      data: first,
      nextOffset: next,
    })
    .mockResolvedValueOnce({
      entryId: card.id,
      runId: card.runId,
      offset: next,
      totalBytes: total,
      sha256: 'a'.repeat(64),
      data: second,
      nextOffset: null,
    })
  expect(await readLargeTimeline('/runs/run/stream', [preview])).toEqual([card])
  const body = JSON.parse(vi.mocked(api).mock.calls[1][1]!.body as string)
  expect(body.offset).toBe(next)
  expect(body.sha256).toBe('a'.repeat(64))
})

it('rejects an inconsistent chunk instead of accepting a partial card', async () => {
  vi.mocked(api).mockResolvedValueOnce({
    entryId: card.id,
    runId: card.runId,
    offset: 0,
    totalBytes: 10,
    sha256: 'a'.repeat(64),
    data: 'x',
    nextOffset: null,
  })
  await expect(readLargeTimeline('/runs/run/stream', [preview])).rejects.toThrow('Invalid timeline content length')
})

it('hydrates a large proposal with its completed identity and parent before applying it', async () => {
  const { readLargeEvents } = await import('../src/large-timeline')
  const proposal = {
    id: 8,
    runId: 'run',
    createdAt: 1,
    type: 'item.completed',
    text: '🦊 detailed plan',
    payload: { item: { id: 'p', type: 'proposed_plan', text: '🦊 detailed plan' }, parent_tool_use_id: 'child', request_id: 'request' },
  }
  const json = JSON.stringify(proposal)
  const split = json.indexOf('🦊')
  const parts = [json.slice(0, split), json.slice(split)]
  const next = new TextEncoder().encode(parts[0]).length
  const total = new TextEncoder().encode(json).length
  for (const [index, data] of parts.entries()) {
    vi.mocked(api).mockResolvedValueOnce({
      eventId: 8,
      runId: 'run',
      offset: index ? next : 0,
      totalBytes: total,
      sha256: 'a'.repeat(64),
      data,
      nextOffset: index ? null : next,
    })
  }

  const preview = { ...proposal, text: '', payload: { _cairnEventContent: true } }
  expect(await readLargeEvents('/runs/run/stream', [preview])).toEqual([proposal])
  expect(vi.mocked(api).mock.calls[1][0]).toBe('/runs/run/event-content')
  expect(JSON.parse(vi.mocked(api).mock.calls[1][1]!.body as string)).toMatchObject({
    eventId: 8,
    runId: 'run',
    offset: next,
    sha256: 'a'.repeat(64),
  })
})

it('rejects a changed proposal identity and incomplete chunks without advancing its input', async () => {
  const { readLargeEvents } = await import('../src/large-timeline')
  const preview = {
    id: 8,
    runId: 'run',
    createdAt: 1,
    type: 'item.completed',
    text: '',
    payload: { _cairnEventContent: true },
  }
  vi.mocked(api).mockResolvedValueOnce({
    eventId: 9,
    runId: 'run',
    offset: 0,
    totalBytes: 10,
    sha256: 'a'.repeat(64),
    data: 'x',
    nextOffset: null,
  })
  await expect(readLargeEvents('/runs/run/stream', [preview])).rejects.toThrow('Event content changed')
  expect(preview.payload._cairnEventContent).toBe(true)
  vi.mocked(api).mockResolvedValueOnce({
    eventId: 8,
    runId: 'run',
    offset: 0,
    totalBytes: 10,
    sha256: 'a'.repeat(64),
    data: 'x',
    nextOffset: null,
  })
  await expect(readLargeEvents('/runs/run/stream', [preview])).rejects.toThrow('Invalid event content length')
})
