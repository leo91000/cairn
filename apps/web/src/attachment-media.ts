import type { InjectionKey, Ref } from 'vue'
import type { ChatAttachment } from '../../../packages/contracts/chats'
import type { AttachmentSummary } from './attachments'
import { onBeforeUnmount, ref, watch } from 'vue'
import {
  api,
  apiBlob,
  ApiError,
  apiUrl,
} from './api'
import { needsSummary } from './attachments'

// Previews and summaries are served with `Cache-Control: no-store`, but an attachment never changes
// for a given id: keep them in memory for the session, fetch only visible items, and share requests.

export function attachmentUrl(attachment: Pick<ChatAttachment, 'id' | 'chatId'>, flag?: 'preview' | 'summary') {
  return apiUrl(`/chats/${encodeURIComponent(attachment.chatId)}/attachments/${encodeURIComponent(attachment.id)}${flag ? `?${flag}` : ''}`)
}

function attachmentPath(attachment: Pick<ChatAttachment, 'id' | 'chatId'>, flag: 'preview' | 'summary') {
  return `/chats/${encodeURIComponent(attachment.chatId)}/attachments/${encodeURIComponent(attachment.id)}?${flag}`
}

interface Entry<T> {
  promise: Promise<T | null>
  value?: T | null
  users: number
}

const PREVIEW_LIMIT = 120
const RETRY_AFTER = 60_000
const previews = new Map<string, Entry<string>>()
const summaries = new Map<string, Entry<AttachmentSummary>>()
const cacheKey = (attachment: Pick<ChatAttachment, 'id' | 'chatId'>) => `${attachment.chatId}/${attachment.id}`

// A missing preview stays missing; other failures (offline, rate limit) may be retried later.
function settle<T>(map: Map<string, Entry<T>>, key: string, entry: Entry<T>, load: () => Promise<T>) {
  entry.promise = load().then((value) => {
    entry.value = value
    return value
  }, (error) => {
    entry.value = null
    if (!(error instanceof ApiError && (error.status === 404 || error.status === 415)))
      setTimeout(() => map.get(key) === entry && !entry.users && map.delete(key), RETRY_AFTER)
    return null
  })
}

function evictPreviews() {
  for (const [key, entry] of previews) {
    if (previews.size <= PREVIEW_LIMIT)
      return
    if (entry.users)
      continue
    previews.delete(key)
    if (entry.value)
      URL.revokeObjectURL(entry.value)
  }
}

function acquire<T>(map: Map<string, Entry<T>>, key: string, load: () => Promise<T>) {
  let entry = map.get(key)
  if (entry) {
    // Most recently used entries move to the end, away from eviction.
    map.delete(key)
    map.set(key, entry)
  }
  else {
    entry = { promise: Promise.resolve(null), users: 0 }
    map.set(key, entry)
    settle(map, key, entry, load)
  }

  entry.users++
  return entry
}

export function loadPreview(attachment: ChatAttachment) {
  const entry = acquire(previews, cacheKey(attachment), async () => URL.createObjectURL(await apiBlob(attachmentPath(attachment, 'preview'))))
  evictPreviews()
  return entry
}

export function loadSummary(attachment: ChatAttachment) {
  return acquire(summaries, cacheKey(attachment), async () => {
    const summary = await api<AttachmentSummary>(attachmentPath(attachment, 'summary'))
    return summary && typeof summary === 'object' ? summary : {}
  })
}

function release<T>(entry: Entry<T> | undefined) {
  if (entry)
    entry.users = Math.max(0, entry.users - 1)
}

/** Cached values available synchronously, so a remounted card does not flash. */
export function cachedSummary(attachment: ChatAttachment) {
  return summaries.get(cacheKey(attachment))?.value ?? undefined
}

// One observer for every attachment card on the page.
let observer: IntersectionObserver | undefined
const visibleCallbacks = new WeakMap<Element, () => void>()

function whenVisible(element: Element, callback: () => void) {
  if (typeof IntersectionObserver === 'undefined') {
    callback()
    return () => {}
  }

  observer ??= new IntersectionObserver((records) => {
    for (const record of records) {
      if (!record.isIntersecting)
        continue
      const visible = visibleCallbacks.get(record.target)
      observer!.unobserve(record.target)
      visibleCallbacks.delete(record.target)
      visible?.()
    }
  }, { rootMargin: '240px' })
  visibleCallbacks.set(element, callback)
  observer.observe(element)
  return () => {
    visibleCallbacks.delete(element)
    observer?.unobserve(element)
  }
}

interface MediaOptions {
  /** The element whose visibility starts the request; immediate when omitted. */
  target?: Ref<HTMLElement | undefined>
  preview?: boolean
  summary?: boolean
}

/**
 * Lazily loads `?preview` (as an object URL) and `?summary` for an uploaded attachment once its
 * card is visible. Local drafts (no chat yet) never request anything.
 */
export function useAttachmentMedia(source: () => ChatAttachment | undefined, options: MediaOptions = {}) {
  const preview = ref<string>()
  const previewFailed = ref(false)
  const summary = ref<AttachmentSummary | null | undefined>()
  let previewEntry: Entry<string> | undefined
  let summaryEntry: Entry<AttachmentSummary> | undefined
  let stopWatching = () => {}
  let generation = 0

  function reset() {
    generation++
    stopWatching()
    stopWatching = () => {}
    release(previewEntry)
    release(summaryEntry)
    previewEntry = undefined
    summaryEntry = undefined
    preview.value = undefined
    previewFailed.value = false
    summary.value = undefined
  }

  function start(attachment: ChatAttachment) {
    const current = generation
    if (options.preview) {
      previewEntry = loadPreview(attachment)
      void previewEntry.promise.then((url) => {
        if (current !== generation)
          return
        preview.value = url ?? undefined
        previewFailed.value = !url
      })
    }

    if (options.summary && needsSummary(attachment)) {
      summaryEntry = loadSummary(attachment)
      void summaryEntry.promise.then((value) => {
        if (current === generation)
          summary.value = value
      })
    }
  }

  watch(() => {
    const attachment = source()
    return attachment?.chatId ? cacheKey(attachment) : ''
  }, (key) => {
    reset()
    const attachment = source()
    if (!key || !attachment)
      return
    summary.value = cachedSummary(attachment)
    const cachedPreview = previews.get(key)?.value
    if (cachedPreview)
      preview.value = cachedPreview
    const element = options.target?.value
    // Everything requested is already known: no need to wait for visibility.
    const known = (!options.preview || previews.get(key)?.value !== undefined)
      && (!options.summary || !needsSummary(attachment) || summaries.get(key)?.value !== undefined)
    if (!options.target || known) {
      start(attachment)
    }
    else if (element) {
      stopWatching = whenVisible(element, () => start(attachment))
    }
    else {
      // The element renders after this watcher; wait for it once.
      const stop = watch(options.target, (value) => {
        if (!value)
          return
        stop()
        stopWatching = whenVisible(value, () => start(attachment))
      }, { flush: 'post' })
      stopWatching = stop
    }
  }, { immediate: true })
  onBeforeUnmount(reset)

  return { preview, previewFailed, summary }
}

// Actions (C').

export const reuseAttachmentKey: InjectionKey<{ chatId: () => string | undefined, reuse: (attachment: ChatAttachment) => void }> = Symbol('reuse-attachment')

export function canShareFiles() {
  if (typeof navigator === 'undefined' || typeof navigator.share !== 'function' || typeof navigator.canShare !== 'function')
    return false
  try {
    return navigator.canShare({ files: [new File([''], 'attachment.txt', { type: 'text/plain' })] })
  }
  catch { return false }
}

export function canCopyImages() {
  return typeof ClipboardItem !== 'undefined' && typeof navigator !== 'undefined' && typeof navigator.clipboard?.write === 'function'
}

export async function attachmentFile(attachment: ChatAttachment) {
  const blob = await apiBlob(`/chats/${encodeURIComponent(attachment.chatId)}/attachments/${encodeURIComponent(attachment.id)}`)
  return new File([blob], attachment.name, { type: blob.type && blob.type !== 'application/octet-stream' ? blob.type : attachment.mediaType || 'application/octet-stream' })
}

export async function shareAttachment(attachment: ChatAttachment) {
  const file = await attachmentFile(attachment)
  try {
    await navigator.share({ files: [file], title: attachment.name })
  }
  catch (error) {
    if ((error as DOMException).name !== 'AbortError')
      throw error
  }
}

async function pngFrom(blob: Blob) {
  const url = URL.createObjectURL(blob)
  try {
    const image = new Image()
    image.decoding = 'async'
    image.src = url
    await image.decode()
    const canvas = document.createElement('canvas')
    canvas.width = image.naturalWidth || 1
    canvas.height = image.naturalHeight || 1
    canvas.getContext('2d')!.drawImage(image, 0, 0)
    return await new Promise<Blob>((resolve, reject) => canvas.toBlob(png => png ? resolve(png) : reject(new Error('This image cannot be copied.')), 'image/png'))
  }
  finally { URL.revokeObjectURL(url) }
}

/** Clipboards accept PNG everywhere; the item is created synchronously to keep the user gesture. */
export async function copyImage(attachment: ChatAttachment, local?: string) {
  const png = (local ? fetch(local).then(response => response.blob()) : attachmentFile(attachment)).then(pngFrom)
  await navigator.clipboard.write([new ClipboardItem({ 'image/png': png })])
}
