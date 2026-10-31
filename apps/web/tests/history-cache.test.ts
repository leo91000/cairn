import type { CachedHistory } from '../src/history-cache'
import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'
import {
  cacheScope,
  clearHistoryCache,
  knownScope,
  peekHistory,
  readHistory,
  writeHistory,
} from '../src/history-cache'

function record(): CachedHistory {
  return {
    version: 2,
    cursor: 2,
    history: 'v2:r:1',
    state: { run: null, chat: null, artifacts: [] },
    entries: [],
    events: [{
      id: 2,
      runId: 'r',
      createdAt: 1,
      type: 'item.completed',
      text: 'saved',
    }],
    savedAt: Date.now(),
  }
}

afterEach(async () => {
  vi.useRealTimers()
  await clearHistoryCache()
})
it('isolates sessions and routes, and copies snapshots before retaining them', async () => {
  const value = record()
  await writeHistory('session', '/chats/a/stream', value)
  value.events[0].text = 'mutated'
  expect((await readHistory('session', '/chats/a/stream'))?.events[0].text).toBe('saved')
  expect(await readHistory('other', '/chats/a/stream')).toBeUndefined()
  expect(await readHistory('session', '/runs/a/stream')).toBeUndefined()
  expect(await cacheScope('session')).not.toContain('session')
})
it('shows a conversation saved in this page without waiting for storage', async () => {
  const scope = await cacheScope('page')
  expect(knownScope('page')).toBe(scope)
  expect(knownScope('another page')).toBeUndefined()
  await writeHistory(scope, '/chats/a/stream', record())
  const shown = peekHistory(scope, '/chats/a/stream')
  expect(shown?.events[0].text).toBe('saved')
  shown!.events[0].text = 'mutated'
  expect(peekHistory(scope, '/chats/a/stream')?.events[0].text).toBe('saved')
  expect(peekHistory(scope, '/chats/b/stream')).toBeUndefined()
  await clearHistoryCache()
  expect(peekHistory(scope, '/chats/a/stream')).toBeUndefined()
})
it('expires, bounds and clears caches even when IndexedDB is unavailable', async () => {
  await writeHistory('s', 'expired', { ...record(), savedAt: Date.now() - 8 * 86400000 })
  expect(await readHistory('s', 'expired')).toBeUndefined()
  await writeHistory('s', 'huge', { ...record(), events: [{ ...record().events[0], text: 'x'.repeat(4 * 1024 * 1024) }] })
  expect(await readHistory('s', 'huge')).toMatchObject({ events: [], hasOlder: true, oldest: 3 })
  for (let i = 0; i < 15; i++)
    await writeHistory('s', String(i), record())
  expect(await readHistory('s', '0')).toBeUndefined()
  expect(await readHistory('s', '14')).toBeDefined()
  await clearHistoryCache()
  expect(await readHistory('s', '14')).toBeUndefined()
})
it('rejects a cursor inconsistent with its saved events', async () => {
  await writeHistory('s', 'bad', { ...record(), cursor: 1 })
  expect(await readHistory('s', 'bad')).toBeUndefined()
})

it('retains previously loaded pages and reading position beyond the initial window', async () => {
  const events = Array.from({ length: 1250 }, (_, i) => ({ ...record().events[0], id: i + 1 }))
  await writeHistory('s', 'window', {
    ...record(),
    cursor: 1250,
    events,
    position: { top: 50, follow: false },
  })
  const saved = await readHistory('s', 'window')
  expect(saved?.events.map(e => e.id)).toEqual(events.map(e => e.id))
  expect(saved).toMatchObject({ cursor: 1250 })
  expect(saved?.position).toEqual({ top: 50, follow: false })
})

it('bounds cached history by bytes and invalidates offsets only when a prefix is evicted', async () => {
  const events = Array.from({ length: 4 }, (_, i) => ({ ...record().events[0], id: i + 1, text: 'x'.repeat(600 * 1024) }))
  await writeHistory('s', 'bytes', {
    ...record(),
    cursor: 4,
    events,
    position: { top: 50, follow: false },
  })
  const saved = await readHistory('s', 'bytes')
  expect(saved?.events.map(e => e.id)).toEqual([3, 4])
  expect(saved).toMatchObject({ oldest: 3, hasOlder: true, cursor: 4 })
  expect(saved?.position).toBeUndefined()
})

it('retains a bounded suffix of timeline cards together with their source events', async () => {
  const events = Array.from({ length: 4 }, (_, i) => ({ ...record().events[0], id: i + 1 }))
  const entries = events.map(event => ({
    id: `reply:${event.id}`,
    runId: 'r',
    position: event.id === 4 ? 1 : event.id,
    source: event.id,
    revision: event.id,
    turn: 1,
    attempt: 1,
    createdAt: 1,
    updatedAt: 1,
    content: {
      kind: 'agentMessage' as const,
      itemId: `reply:${event.id}`,
      state: 'done' as const,
      text: 'x'.repeat(600 * 1024),
    },
  }))
  await writeHistory('s', 'cards', {
    ...record(),
    cursor: 4,
    events,
    entries,
    position: { top: 50, follow: false },
  })
  const saved = await readHistory('s', 'cards')
  expect(saved?.events.map(event => event.id)).toEqual([3, 4])
  expect(saved?.entries.map(card => card.source)).toEqual([3, 4])
  expect(saved).toMatchObject({ oldest: 3, hasOlder: true, cursor: 4 })
  expect(saved?.position).toBeUndefined()
})
