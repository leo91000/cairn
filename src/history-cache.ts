import type { RunEvent } from '../shared/contracts'
import type { LiveState } from '../shared/live'

export interface ReadingPosition { top: number, follow: boolean }
export interface CachedHistory {
  oldest?: number
  hasOlder?: boolean
  version: 1
  cursor: number
  history: string
  state: LiveState
  events: RunEvent[]
  savedAt: number
  position?: ReadingPosition
}
const memory = new Map<string, CachedHistory>()
const MAX_ENTRY = 4 * 1024 * 1024
const MAX_TOTAL = 20 * 1024 * 1024
const MAX_COUNT = 12
const TTL = 7 * 24 * 60 * 60 * 1000
let epoch = 0
let serial = Promise.resolve()

export async function cacheScope(session: string) {
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(session))
  return Array.from(new Uint8Array(bytes), n => n.toString(16).padStart(2, '0')).join('')
}

function valid(value: CachedHistory): boolean {
  return value?.version === 1 && typeof value.history === 'string' && value.history.startsWith('v1:')
    && Number.isSafeInteger(value.cursor) && value.cursor >= 0 && Array.isArray(value.events)
    && !!value.state && Array.isArray(value.state.artifacts)
    && (value.state.run === null || typeof value.state.run?.id === 'string')
    && (value.state.chat === null || (typeof value.state.chat?.id === 'string' && Array.isArray(value.state.chat.messages)))
    && (!value.position || (Number.isFinite(value.position.top) && value.position.top >= 0 && typeof value.position.follow === 'boolean'))
    && Number.isFinite(value.savedAt) && Date.now() - value.savedAt < TTL
    && value.events.every(e => !!e && typeof e.text === 'string' && Number.isSafeInteger(e.id) && e.id > 0 && e.id <= value.cursor && typeof e.type === 'string')
}

function db() {
  return new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open('leo-history-v1', 1)
    request.onupgradeneeded = () => request.result.createObjectStore('entries', { keyPath: 'key' })
    request.onsuccess = () => resolve(request.result)
    request.onerror = () => reject(request.error)
  })
}

async function transaction<T>(mode: IDBTransactionMode, work: (store: IDBObjectStore, result: (value: T) => void) => void) {
  const database = await db()
  try {
    return await new Promise<T>((resolve, reject) => {
      const tx = database.transaction('entries', mode)
      let value: T
      tx.oncomplete = () => resolve(value)
      tx.onerror = tx.onabort = () => reject(tx.error)
      work(tx.objectStore('entries'), (result) => {
        value = result
      })
    })
  }
  finally { database.close() }
}

function enqueue(work: () => Promise<void>) {
  serial = serial.then(work).catch(() => {}) // Storage denial/quota cannot break live messages.
  return serial
}

export async function readHistory(scope: string, path: string): Promise<CachedHistory | undefined> {
  const key = `${scope}:${path}`
  const current = epoch
  let value = memory.get(key)
  if (!value) {
    try {
      const text = await transaction<string | undefined>('readonly', (store, done) => {
        const r = store.get(key)
        r.onsuccess = () => done(r.result?.text)
      })
      if (text && text.length * 2 <= MAX_ENTRY)
        value = JSON.parse(text)
    }
    catch { /* Cache miss; the stream remains authoritative. */ }
  }

  if (current !== epoch || !value || !valid(value))
    return undefined
  memory.delete(key)
  memory.set(key, value)
  trimMemory()
  return structuredClone(value)
}

function trimMemory() {
  let total = 0
  for (const [index, [id, item]] of [...memory].reverse().entries()) {
    total += JSON.stringify(item).length * 2
    if (total > MAX_TOTAL || index >= MAX_COUNT)
      memory.delete(id)
  }
}

export function writeHistory(scope: string, path: string, value: CachedHistory) {
  const key = `${scope}:${path}`
  const current = epoch
  if (!valid(value))
    return removeHistory(scope, path)
  value = recentHistory(value)
  const text = JSON.stringify(value)
  if (text.length * 2 > MAX_ENTRY)
    return removeHistory(scope, path)
  memory.delete(key)
  memory.set(key, JSON.parse(text))
  trimMemory()
  return enqueue(async () => {
    if (current !== epoch)
      return
    await transaction<void>('readwrite', (store) => {
      store.put({ key, text, savedAt: value.savedAt })
      const request = store.getAll()
      request.onsuccess = () => {
        let size = 0
        const entries = request.result.sort((a, b) => b.savedAt - a.savedAt)
        entries.forEach((entry, index) => {
          size += entry.text.length * 2
          if (index >= MAX_COUNT || size > MAX_TOTAL || Date.now() - entry.savedAt >= TTL)
            store.delete(entry.key)
        })
      }
    })
  })
}

export function removeHistory(scope: string, path: string) {
  const key = `${scope}:${path}`
  memory.delete(key)
  return enqueue(() => transaction<void>('readwrite', (store) => {
    store.delete(key)
  }))
}

export function clearHistoryCache() {
  epoch++
  memory.clear()
  return enqueue(() => transaction<void>('readwrite', (store) => {
    store.clear()
  }))
}

export function recentHistory(value: CachedHistory): CachedHistory {
  let size = JSON.stringify(value.state).length * 2 + 4096
  const ordered = value.events.toSorted((a, b) => a.id - b.id)
  let start = ordered.length
  while (start > 0 && value.events.length - start < 200) {
    const bytes = JSON.stringify(ordered[start - 1]).length * 2 + 2
    if (size + bytes > 3 * 1024 * 1024)
      break
    size += bytes
    start--
  }

  if (!start)
    return value
  const oldest = ordered[start]?.id ?? value.cursor + 1
  const events = value.events.filter(e => e.id >= oldest)
  return {
    ...value,
    events,
    oldest,
    hasOlder: true,
    position: undefined,
  }
}
