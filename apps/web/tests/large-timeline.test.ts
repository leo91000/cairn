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

const preview = {
  id: 'message:1:é',
  position: 1,
  revision: 2,
  source: 2,
  kind: 'agentMessage',
  text: '',
}
const card: TimelineEntry = {
  id: preview.id,
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
      entryId: preview.id,
      offset: 0,
      totalBytes: total,
      sha256: 'a'.repeat(64),
      data: first,
      nextOffset: next,
    })
    .mockResolvedValueOnce({
      entryId: preview.id,
      offset: next,
      totalBytes: total,
      sha256: 'a'.repeat(64),
      data: second,
      nextOffset: null,
    })
  expect(await readLargeTimeline('/runs/run/stream', [preview])).toEqual([card])
  expect(vi.mocked(api).mock.calls[1][0]).toContain(`offset=${next}`)
  expect(vi.mocked(api).mock.calls[1][0]).toContain(`sha256=${'a'.repeat(64)}`)
})

it('rejects an inconsistent chunk instead of accepting a partial card', async () => {
  vi.mocked(api).mockResolvedValueOnce({
    entryId: preview.id,
    offset: 0,
    totalBytes: 10,
    sha256: 'a'.repeat(64),
    data: 'x',
    nextOffset: null,
  })
  await expect(readLargeTimeline('/runs/run/stream', [preview])).rejects.toThrow('Invalid timeline content length')
})
