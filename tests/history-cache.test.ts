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
  readHistory,
  writeHistory,
} from '../src/history-cache'

function record(): CachedHistory {
  return {
    version: 1,
    cursor: 2,
    history: 'v1:r:1',
    state: { run: null, chat: null, artifacts: [] },
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

it('keeps a recent window and a backwards boundary without retaining stale offsets', async () => {
  const events = Array.from({ length: 250 }, (_, i) => ({ ...record().events[0], id: i + 1 }))
  await writeHistory('s', 'window', {
    ...record(),
    cursor: 250,
    events,
    position: { top: 50, follow: false },
  })
  const saved = await readHistory('s', 'window')
  expect(saved?.events.map(e => e.id)).toEqual(events.slice(50).map(e => e.id))
  expect(saved).toMatchObject({ oldest: 51, hasOlder: true, cursor: 250 })
  expect(saved?.position).toBeUndefined()
})
