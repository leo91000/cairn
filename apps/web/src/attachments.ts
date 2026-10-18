import type { ChatAttachment } from '../../../packages/contracts/chats'

// Chat attachment presentation shared with Android: keep this table, the extension rule and the
// media-type fallback identical on both platforms.
export type FileFamily = 'image' | 'pdf' | 'sheet' | 'doc' | 'slides' | 'code' | 'text' | 'archive' | 'audio' | 'video' | 'other'

export const familyExtensions: Record<Exclude<FileFamily, 'other'>, readonly string[]> = {
  image: ['png', 'jpg', 'jpeg', 'gif', 'webp', 'svg', 'heic', 'heif', 'avif', 'bmp'],
  pdf: ['pdf'],
  sheet: ['csv', 'tsv', 'xls', 'xlsx', 'ods', 'numbers'],
  doc: ['doc', 'docx', 'odt', 'rtf', 'pages'],
  slides: ['ppt', 'pptx', 'odp', 'key'],
  code: ['js', 'mjs', 'cjs', 'ts', 'tsx', 'jsx', 'py', 'rs', 'kt', 'kts', 'java', 'go', 'rb', 'php', 'c', 'h', 'cc', 'cpp', 'hpp', 'cs', 'swift', 'sh', 'bash', 'zsh', 'fish', 'sql', 'html', 'htm', 'css', 'scss', 'sass', 'less', 'vue', 'svelte', 'json', 'jsonl', 'yaml', 'yml', 'toml', 'xml', 'ini', 'gradle', 'lua', 'dart', 'scala', 'r', 'ps1', 'dockerfile'],
  text: ['txt', 'log', 'md', 'markdown', 'rst'],
  archive: ['zip', 'tar', 'gz', 'tgz', 'bz2', 'xz', '7z', 'rar', 'zst'],
  audio: ['mp3', 'wav', 'm4a', 'ogg', 'oga', 'flac', 'aac', 'opus'],
  video: ['mp4', 'mov', 'webm', 'mkv', 'avi', 'm4v'],
}

// Images show a thumbnail instead of a coloured tile; they fall back to the neutral colour.
export const familyColors: Record<FileFamily, string | null> = {
  image: null,
  pdf: '#D2452F',
  sheet: '#1F7A4D',
  doc: '#2D62B3',
  slides: '#C4581F',
  code: '#3B3A46',
  text: '#8986A4',
  archive: '#B9822A',
  audio: '#7A4FB0',
  video: '#B23A6E',
  other: '#6D6A78',
}

const byExtension = new Map<string, FileFamily>(Object.entries(familyExtensions).flatMap(([family, extensions]) => extensions.map(extension => [extension, family as FileFamily])))

export const MAX_ATTACHMENTS = 8
export const MAX_FILE_SIZE = 10 * 1024 * 1024
export const MAX_MESSAGE_SIZE = 40 * 1024 * 1024

type Named = Pick<ChatAttachment, 'name'> & Partial<Pick<ChatAttachment, 'kind' | 'mediaType'>>

/** Lowercased text after the last dot, when the base is non-empty and it is 1–8 alphanumerics. */
export function extension(name: string) {
  const dot = name.lastIndexOf('.')
  if (dot <= 0)
    return ''
  const value = name.slice(dot + 1)
  return /^[a-z0-9]{1,8}$/i.test(value) ? value.toLowerCase() : ''
}

export function family(attachment: Named): FileFamily {
  if (attachment.kind === 'image')
    return 'image'
  const known = byExtension.get(extension(attachment.name))
  if (known)
    return known
  const media = (attachment.mediaType ?? '').toLowerCase()
  if (media.startsWith('image/'))
    return 'image'
  if (media === 'application/pdf')
    return 'pdf'
  if (media.startsWith('audio/'))
    return 'audio'
  if (media.startsWith('video/'))
    return 'video'
  if (media.startsWith('text/'))
    return 'text'
  return 'other'
}

export function isImage(attachment: Named) {
  return family(attachment) === 'image'
}

export function isSvg(attachment: Named) {
  return extension(attachment.name) === 'svg' || attachment.mediaType?.toLowerCase() === 'image/svg+xml'
}

/** Badge label: the extension uppercased, at most 4 characters; empty means a generic file icon. */
export function badgeLabel(name: string) {
  return extension(name).toUpperCase().slice(0, 4)
}

/** The type named in subtitles: the badge label, or "File" without an extension (as on Android). */
export function typeLabel(name: string) {
  return badgeLabel(name) || 'File'
}

export function badgeColor(attachment: Named) {
  return familyColors[family(attachment)] ?? familyColors.other!
}

/** Base truncates with an end ellipsis; the `.ext` part always stays visible. */
export function splitName(name: string) {
  const ext = extension(name)
  return ext ? { base: name.slice(0, name.length - ext.length - 1), ext: name.slice(name.length - ext.length - 1) } : { base: name, ext: '' }
}

export function size(bytes: number) {
  return bytes < 1024 ? `${bytes} B` : bytes < 1024 * 1024 ? `${Math.ceil(bytes / 1024)} KB` : `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

export interface AttachmentSummary {
  pages?: number
  lines?: number
  excerpt?: string
}

/** Previewable as a B3 document card: PDF page image, or an excerpt for csv/tsv, code and text. */
export function documentPreview(attachment: Named): 'page' | 'excerpt' | null {
  const kind = family(attachment)
  if (kind === 'pdf')
    return 'page'
  if (kind === 'code' || kind === 'text' || (kind === 'sheet' && ['csv', 'tsv'].includes(extension(attachment.name))))
    return 'excerpt'
  return null
}

/** Only these files have something in `?summary` worth a request. */
export function needsSummary(attachment: Named) {
  return documentPreview(attachment) !== null
}

export function detail(attachment: Named, summary?: AttachmentSummary | null) {
  const kind = family(attachment)
  if (kind === 'pdf' && typeof summary?.pages === 'number')
    return `${summary.pages.toLocaleString('en-US')} ${summary.pages === 1 ? 'page' : 'pages'}`
  if ((kind === 'sheet' || kind === 'text' || kind === 'code') && typeof summary?.lines === 'number')
    return `${summary.lines.toLocaleString('en-US')} ${summary.lines === 1 ? 'line' : 'lines'}`
  return ''
}

/** `LABEL · details · size`, skipping parts that are unknown. */
export function subtitle(attachment: Named & Pick<ChatAttachment, 'size'>, summary?: AttachmentSummary | null, options: { label?: boolean, details?: boolean } = {}) {
  return [
    options.label === false ? '' : typeLabel(attachment.name),
    options.details === false ? '' : detail(attachment, summary),
    size(attachment.size),
  ].filter(Boolean).join(' · ')
}

export interface Mosaic {
  layout: 'single' | 'pair' | 'triple' | 'grid'
  /** Tiles drawn; the last one carries `hidden` as `+N` when positive. */
  shown: number
  hidden: number
}

export function mosaic(count: number): Mosaic {
  if (count <= 1)
    return { layout: 'single', shown: count, hidden: 0 }
  if (count === 2)
    return { layout: 'pair', shown: 2, hidden: 0 }
  if (count === 3)
    return { layout: 'triple', shown: 3, hidden: 0 }
  return { layout: 'grid', shown: 4, hidden: count - 4 }
}

/** Sent-message grouping: images form the mosaic, previewable files document cards, the rest compact cards. */
export function arrange<T extends Named>(attachments: T[]) {
  const images: T[] = []
  const documents: T[] = []
  const files: T[] = []
  for (const attachment of attachments) {
    if (isImage(attachment))
      images.push(attachment)
    else if (documentPreview(attachment))
      documents.push(attachment)
    else
      files.push(attachment)
  }

  return { images, documents, files }
}

/** A draft item's own state (A'): being uploaded, failed during send (retried next time), or refused locally. */
export interface DraftState {
  status: 'uploading' | 'failed' | 'rejected'
  /** 0–1 when the upload reports progress. */
  progress?: number
  error?: string
}

export interface Selection {
  /** A global problem: nothing from this selection is added. */
  error?: string
  /** One entry per selected file: the reason it is rejected, or `null` when it can be uploaded. */
  rejected: (string | null)[]
}

/**
 * Validates newly selected files against the draft. Over 8 files stays a global error; a file over
 * 10 MB, or one that would take the message past 40 MB, is kept but rejected with a reason.
 */
export function validateSelection(draft: { size: number, rejected?: boolean }[], selected: { size: number }[]): Selection {
  if (draft.length + selected.length > MAX_ATTACHMENTS)
    return { error: `Attach up to ${MAX_ATTACHMENTS} files per message.`, rejected: selected.map(() => null) }
  let total = draft.reduce((sum, item) => sum + (item.rejected ? 0 : item.size), 0)
  const rejected = selected.map((file) => {
    if (file.size > MAX_FILE_SIZE)
      return `Too large (${size(file.size)} > 10 MB)`
    if (total + file.size > MAX_MESSAGE_SIZE)
      return 'Message would exceed 40 MB'
    total += file.size
    return null
  })

  return { rejected }
}
