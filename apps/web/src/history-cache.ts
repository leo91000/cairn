import type { ChatView } from '../../../packages/contracts/chats'
import type { RunEvent } from '../../../packages/contracts/contracts'
import type { LiveState, TimelineEntry } from '../../../packages/contracts/live'
import { withChatRun } from './live-state'

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
// Conversations deleted while this page is open: a reader leaving one never saves it again.
const forgotten = new Set<string>()
const scopes = new Map<string, string>()
const MAX_ENTRY = 4 * 1024 * 1024
const MAX_HISTORY = 3 * 1024 * 1024
const MAX_TOTAL = 20 * 1024 * 1024
// Saved conversations hold their own history only: the list is saved once, apart.
const MAX_COUNT = 60
const TTL = 7 * 24 * 60 * 60 * 1000
// Saved conversations are keyed by stream path, which always starts with a slash.
const LIST = 'list'
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
  return value?.version === 2 && typeof value.history === 'string' && value.history.startsWith('v2:')
    && Number.isSafeInteger(value.cursor) && value.cursor >= 0 && Array.isArray(value.events) && Array.isArray(value.entries)
    && !!value.state && Array.isArray(value.state.artifacts)
    && (value.state.run === null || typeof value.state.run?.id === 'string')
    && (value.state.chat === null || (typeof value.state.chat?.id === 'string' && Array.isArray(value.state.chat.messages)))
    && (!value.position || (Number.isFinite(value.position.top) && value.position.top >= 0 && typeof value.position.follow === 'boolean'))
    && Number.isFinite(value.savedAt) && Date.now() - value.savedAt < TTL
    && value.events.every(e => !!e && typeof e.text === 'string' && Number.isSafeInteger(e.id) && e.id > 0 && e.id <= value.cursor && typeof e.type === 'string')
    && value.entries.every(card => !!card && typeof card.id === 'string' && Number.isSafeInteger(card.position) && Number.isSafeInteger(card.revision) && typeof card.content?.kind === 'string')
}

// Saved conversations indexed by age and size for bounded eviction. Version 3 holds
// conversations without the list or the chat's duplicated run, and the list apart: a tab of
// an older bundle, with its own eviction and invalidation, keeps to version 2.
const DATABASE = 'cairn-history-v3'

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

// Newest first: deletes entries beyond the count, the total size or their age. The saved list
// is not one of the counted conversations: it tells which saved ones were deleted meanwhile.
function evict(store: IDBObjectStore) {
  let count = 0
  let total = 0
  const request = store.index('age').openKeyCursor(null, 'prev')
  request.onsuccess = () => {
    const cursor = request.result
    if (!cursor)
      return
    const [savedAt, size] = cursor.key as [number, number]
    if (!String(cursor.primaryKey).endsWith(`:${LIST}`))
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
  let count = 0
  for (const [id, item] of [...memory].reverse()) {
    total += item.size
    if (!id.endsWith(`:${LIST}`))
      count++
    if (total > MAX_TOTAL || count > MAX_COUNT)
      memory.delete(id)
  }
}

function parse(text: string | undefined) {
  if (!text || text.length * 2 > MAX_ENTRY)
    return undefined
  const value = JSON.parse(text) as CachedHistory
  return valid(value) ? { ...value, state: withChatRun(value.state) } : undefined
}

/**
 * A conversation is saved without the list its stream may carry, which is saved once by
 * `writeChatList`, and without its chat's run, which is the state's own `run`.
 */
function saved(value: CachedHistory): CachedHistory {
  const { chats: _chats, ...state } = value.state
  return {
    ...value,
    state: { ...state, chat: state.chat && { ...state.chat, run: undefined as never } },
  }
}

/** Whether a conversation is held in this page, without reading it. */
export function inMemory(scope: string, path: string) {
  return memory.has(`${scope}:${path}`)
}

/** A conversation read in this page, available without waiting for storage. */
export function peekHistory(scope: string, path: string): CachedHistory | undefined {
  if (forgotten.has(`${scope}:${path}`))
    return undefined
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
  // A read started before its deletion must not bring the copy back.
  if (forgotten.has(key))
    return undefined
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
    if (current !== epoch || !value || forgotten.has(key))
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
  if (!valid(value) || forgotten.has(key))
    return removeHistory(scope, path)
  value = saved(value)
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

export interface SavedChatList { chats: ChatView[], planTurnsEnabled: boolean, savedAt: number }

/** The conversation list last received, shown until the server's list arrives. */
export async function readChatList(scope: string): Promise<SavedChatList | undefined> {
  const key = `${scope}:${LIST}`
  const current = epoch
  let text = memory.get(key)?.text
  if (text === undefined) {
    try {
      text = await transaction<string | undefined>('readonly', (store, done) => {
        const r = store.get(key)
        r.onsuccess = () => done(r.result?.text)
      })
    }
    catch { /* Cache miss: the stream remains authoritative. */ }
  }

  try {
    const value = text === undefined ? undefined : JSON.parse(text) as SavedChatList
    if (current !== epoch || !value || !Array.isArray(value.chats) || !Number.isFinite(value.savedAt) || Date.now() - value.savedAt >= TTL)
      return undefined
    return value
  }
  catch {
    return undefined
  }
}

export function writeChatList(scope: string, value: SavedChatList) {
  const key = `${scope}:${LIST}`
  const current = epoch
  const text = JSON.stringify(value)
  // A list too large to save must not leave an older one to be restored instead.
  if (text.length * 2 > MAX_ENTRY) {
    memory.delete(key)
    return enqueue(() => transaction<void>('readwrite', (store) => {
      store.delete(key)
    }))
  }

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

/** Removes a deleted conversation's saved copy for good: this page never saves it again. */
export function forgetHistory(scope: string, path: string) {
  forgotten.add(`${scope}:${path}`)
  return removeHistory(scope, path)
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
  forgotten.clear()
  return enqueue(() => transaction<void>('readwrite', (store) => {
    store.clear()
  }))
}

/**
 * Drops every saved run log of a session. A run does not name its conversation, so a deleted
 * conversation's earlier runs cannot be told apart; a run log opens from its stream again.
 */
export function removeRunLogs(scope: string) {
  const prefix = `${scope}:/runs/`
  for (const key of [...memory.keys()]) {
    if (key.startsWith(prefix))
      memory.delete(key)
  }

  return enqueue(() => transaction<void>('readwrite', (store) => {
    store.delete(IDBKeyRange.bound(prefix, `${prefix}\uFFFF`))
  }))
}

/** On sign-out, the saved conversations of earlier bundles go too: their tabs are signed out. */
export function clearEarlierHistory() {
  try {
    indexedDB.deleteDatabase('cairn-history-v2')
  }
  catch { /* Storage may be unavailable; nothing was saved then. */ }
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
