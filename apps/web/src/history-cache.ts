import type { RunEvent } from '../../../packages/contracts/contracts'
import type { LiveState, TimelineEntry } from '../../../packages/contracts/live'

export interface ReadingPosition { top: number, follow: boolean }
export interface CachedHistory {
  oldest?: number
  hasOlder?: boolean
  version: 2
  cursor: number
  history: string
  state: LiveState
  events: RunEvent[]
  /** Timeline cards; cards are interpreted by the server, never derived from events. */
  entries: TimelineEntry[]
  savedAt: number
  position?: ReadingPosition
}
// Serialized entries: a read parses its own copy, and trimming needs no re-serialization.
const memory = new Map<string, { text: string, size: number }>()
const scopes = new Map<string, string>()
const MAX_ENTRY = 4 * 1024 * 1024
const MAX_HISTORY = 3 * 1024 * 1024
const MAX_TOTAL = 20 * 1024 * 1024
const MAX_COUNT = 12
const TTL = 7 * 24 * 60 * 60 * 1000
let epoch = 0
let serial = Promise.resolve()

export async function cacheScope(session: string) {
  const known = scopes.get(session)
  if (known)
    return known
  const bytes = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(session))
  const scope = Array.from(new Uint8Array(bytes), n => n.toString(16).padStart(2, '0')).join('')
  scopes.set(session, scope)
  return scope
}

/** The scope of a session already hashed in this page, without waiting for the digest. */
export function knownScope(session: string) {
  return scopes.get(session)
}

function valid(value: CachedHistory): boolean {
  // The Fil has no reply representation: its legacy identity describes only the list.
  const legacyList = value?.history === 'v1::0' && value.cursor === 0
    && value.state?.run === null && value.state.chat === null && Array.isArray(value.state.chats)
    && value.state.artifacts?.length === 0 && value.events?.length === 0 && value.entries?.length === 0
    && (value.oldest === undefined || value.oldest === 0)
    && (value.hasOlder === undefined || value.hasOlder === false)

  return value?.version === 2 && typeof value.history === 'string' && (value.history.startsWith('v2:') || legacyList)
    && Number.isSafeInteger(value.cursor) && value.cursor >= 0 && Array.isArray(value.events) && Array.isArray(value.entries)
    && !!value.state && Array.isArray(value.state.artifacts)
    && (value.state.run === null || typeof value.state.run?.id === 'string')
    && (value.state.chat === null || (typeof value.state.chat?.id === 'string' && Array.isArray(value.state.chat.messages)))
    && (!value.position || (Number.isFinite(value.position.top) && value.position.top >= 0 && typeof value.position.follow === 'boolean'))
    && Number.isFinite(value.savedAt) && Date.now() - value.savedAt < TTL
    && value.events.every(e => !!e && typeof e.text === 'string' && Number.isSafeInteger(e.id) && e.id > 0 && e.id <= value.cursor && typeof e.type === 'string')
    && value.entries.every(card => !!card && typeof card.id === 'string' && Number.isSafeInteger(card.position) && Number.isSafeInteger(card.revision) && typeof card.content?.kind === 'string')
}

// Saved conversations indexed by age and size for bounded eviction.
const DATABASE = 'cairn-history-v2'

function db() {
  return new Promise<IDBDatabase>((resolve, reject) => {
    const request = indexedDB.open(DATABASE, 1)
    request.onupgradeneeded = () => {
      const store = request.result.createObjectStore('entries', { keyPath: 'key' })
      store.createIndex('age', ['savedAt', 'size'])
    }

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

// Newest first: deletes entries beyond the count, the total size or their age.
function evict(store: IDBObjectStore) {
  let count = 0
  let total = 0
  const request = store.index('age').openKeyCursor(null, 'prev')
  request.onsuccess = () => {
    const cursor = request.result
    if (!cursor)
      return
    const [savedAt, size] = cursor.key as [number, number]
    count++
    total += size
    if (count > MAX_COUNT || total > MAX_TOTAL || Date.now() - savedAt >= TTL)
      store.delete(cursor.primaryKey)
    cursor.continue()
  }
}

function remember(key: string, text: string) {
  memory.delete(key)
  memory.set(key, { text, size: text.length * 2 })
  let total = 0
  for (const [index, [id, item]] of [...memory].reverse().entries()) {
    total += item.size
    if (total > MAX_TOTAL || index >= MAX_COUNT)
      memory.delete(id)
  }
}

function parse(text: string | undefined) {
  if (!text || text.length * 2 > MAX_ENTRY)
    return undefined
  const value = JSON.parse(text) as CachedHistory
  return valid(value) ? value : undefined
}

/** A conversation read in this page, available without waiting for storage. */
export function peekHistory(scope: string, path: string): CachedHistory | undefined {
  try {
    return parse(memory.get(`${scope}:${path}`)?.text)
  }
  catch {
    return undefined
  }
}

export async function readHistory(scope: string, path: string): Promise<CachedHistory | undefined> {
  const key = `${scope}:${path}`
  const current = epoch
  let text = memory.get(key)?.text
  if (text === undefined) {
    try {
      text = await transaction<string | undefined>('readonly', (store, done) => {
        const r = store.get(key)
        r.onsuccess = () => done(r.result?.text)
      })
    }
    catch { /* Cache miss; the stream remains authoritative. */ }
  }

  try {
    const value = parse(text)
    if (current !== epoch || !value)
      return undefined
    remember(key, text!)
    return value
  }
  catch {
    return undefined
  }
}

export function writeHistory(scope: string, path: string, value: CachedHistory) {
  const key = `${scope}:${path}`
  const current = epoch
  if (!valid(value))
    return removeHistory(scope, path)
  let text = JSON.stringify(value)
  // Most conversations fit: only a long one pays for measuring each event.
  if (text.length * 2 + 4096 > MAX_HISTORY)
    text = JSON.stringify(recentHistory(value))
  if (text.length * 2 > MAX_ENTRY)
    return removeHistory(scope, path)
  remember(key, text)
  return enqueue(async () => {
    if (current !== epoch)
      return
    await transaction<void>('readwrite', (store) => {
      store.put({
        key,
        text,
        savedAt: value.savedAt,
        size: text.length * 2,
      })
      evict(store)
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
  const activeWaits = new Set(value.entries.filter(card => card.content.kind === 'notice'
    && card.content.notice.code === 'backgroundTasks'
    && card.content.notice.waitingSince !== undefined))
  for (const card of activeWaits)
    size += JSON.stringify(card).length * 2 + 2
  const groups = new Map<number, number>()
  for (const event of value.events)
    groups.set(event.id, (groups.get(event.id) ?? 0) + JSON.stringify(event).length * 2 + 2)
  for (const card of value.entries) {
    if (!activeWaits.has(card))
      groups.set(card.source, (groups.get(card.source) ?? 0) + JSON.stringify(card).length * 2 + 2)
  }

  const ordered = [...groups.keys()].toSorted((a, b) => a - b)
  let start = ordered.length
  while (start > 0) {
    const bytes = groups.get(ordered[start - 1]!)!
    if (size + bytes > MAX_HISTORY)
      break
    size += bytes
    start--
  }

  if (!start)
    return value
  const oldest = ordered[start] ?? value.cursor + 1
  const events = value.events.filter(e => e.id >= oldest)
  return {
    ...value,
    events,
    entries: value.entries.filter(card => card.source >= oldest || activeWaits.has(card)),
    oldest,
    hasOlder: true,
    position: undefined,
  }
}
