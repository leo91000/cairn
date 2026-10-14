import type {
  SkillArchiveChunk,
  SkillChange,
  SkillFile,
  SkillLimits,
} from '../../../packages/contracts/contracts'

// Multi-file skill editing: paths, file kinds, the local draft, the batch sent on save,
// staged uploads and chunked archive downloads. See docs/SKILLS.md.

export const skillManifest = 'SKILL.md'
// Used until a bundle response provides the installation's own limits.
export const defaultSkillLimits: SkillLimits = {
  fileBytes: 4 * 1024 * 1024,
  bundleBytes: 16 * 1024 * 1024,
  files: 500,
  uploadBytes: 5 * 1024 * 1024,
}
// Installation JSON bodies are capped at 150 KB: binaries, larger text files, and the largest
// files of a batch that would pass `inlineRequestBytes` are staged as raw uploads first.
export const inlineFileBytes = 32 * 1024
export const inlineRequestBytes = 120_000
const pathBytes = 240
const pathSegments = 8

export function formatBytes(value: number) {
  if (value < 1024)
    return `${value} B`

  const units = ['KB', 'MB', 'GB']
  let size = value / 1024
  let unit = 0
  while (size >= 1024 && unit < units.length - 1) {
    size /= 1024
    unit += 1
  }

  const rounded = size >= 100 ? Math.round(size) : Math.round(size * 10) / 10
  return `${rounded} ${units[unit]}`
}

export function plural(count: number, singular: string, many = `${singular}s`) {
  return `${count} ${count === 1 ? singular : many}`
}

const encoder = new TextEncoder()

export function utf8Bytes(text: string) {
  return encoder.encode(text).length
}

export function bytesToBase64(bytes: Uint8Array) {
  let binary = ''
  const chunk = 0x8000
  for (let index = 0; index < bytes.length; index += chunk)
    binary += String.fromCharCode(...bytes.subarray(index, index + chunk))

  return btoa(binary)
}

export function base64ToBytes(value: string): Uint8Array<ArrayBuffer> {
  const binary = atob(value)
  const bytes = new Uint8Array(binary.length)
  for (let index = 0; index < binary.length; index += 1)
    bytes[index] = binary.charCodeAt(index)

  return bytes
}

// Matches the backend: text is valid UTF-8 without NUL bytes.
export function decodeText(bytes: Uint8Array) {
  if (bytes.includes(0))
    return null

  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes)
  }
  catch {
    return null
  }
}

export function base64Bytes(value: string) {
  const padding = value.endsWith('==') ? 2 : value.endsWith('=') ? 1 : 0
  return Math.max(0, Math.floor(value.length * 3 / 4) - padding)
}

// Accepts what people type: surrounding spaces, `./`, leading or doubled slashes, backslashes.
export function normalizePath(input: string) {
  return input
    .trim()
    .replaceAll('\\', '/')
    .split('/')
    .filter(segment => segment !== '' && segment !== '.')
    .join('/')
}

export function validateSkillPath(path: string) {
  if (!path)
    return 'Enter a file name.'
  const segments = path.split('/')
  if (segments.includes('..'))
    return 'Paths stay inside the skill folder.'
  if (segments.some(segment => segment.startsWith('.')))
    return 'File and folder names cannot start with a dot.'
  if (segments.length > pathSegments)
    return `Paths have at most ${pathSegments} levels.`
  if (utf8Bytes(path) > pathBytes)
    return `Paths are limited to ${pathBytes} bytes.`
  if ([...path].some(character => character.charCodeAt(0) < 32))
    return 'File names cannot contain control characters.'

  return null
}

export function basename(path: string) {
  return path.slice(path.lastIndexOf('/') + 1)
}

export function dirname(path: string) {
  const index = path.lastIndexOf('/')
  return index < 0 ? '' : path.slice(0, index)
}

export function joinPath(folder: string, name: string) {
  return normalizePath(folder ? `${folder}/${name}` : name)
}

export function isInside(path: string, folder: string) {
  return path.startsWith(`${folder}/`)
}

export type SkillFileKind = 'manifest' | 'markdown' | 'script' | 'image' | 'text' | 'binary'

const imageTypes: Record<string, string> = {
  png: 'image/png',
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  gif: 'image/gif',
  webp: 'image/webp',
  avif: 'image/avif',
  bmp: 'image/bmp',
  ico: 'image/x-icon',
  svg: 'image/svg+xml',
}
const scriptExtensions = new Set(['sh', 'bash', 'zsh', 'fish', 'py', 'rb', 'pl', 'js', 'mjs', 'cjs', 'ts', 'php', 'lua', 'ps1'])

function extension(path: string) {
  const name = basename(path)
  const index = name.lastIndexOf('.')
  return index <= 0 ? '' : name.slice(index + 1).toLowerCase()
}

export function imageType(path: string) {
  return imageTypes[extension(path)]
}

export function isMarkdown(path: string) {
  return ['md', 'markdown', 'mdx'].includes(extension(path))
}

export function skillFileKind(file: { path: string, text: boolean, executable: boolean }): SkillFileKind {
  if (file.path === skillManifest)
    return 'manifest'
  if (imageType(file.path) && (!file.text || extension(file.path) === 'svg'))
    return 'image'
  if (!file.text)
    return 'binary'
  if (file.executable || scriptExtensions.has(extension(file.path)))
    return 'script'
  if (isMarkdown(file.path))
    return 'markdown'

  return 'text'
}

export interface SkillTreeFolder<T> {
  type: 'folder'
  name: string
  path: string
  depth: number
  children: SkillTreeNode<T>[]
  size: number
  files: number
}
export interface SkillTreeFile<T> {
  type: 'file'
  name: string
  path: string
  depth: number
  file: T
}
export type SkillTreeNode<T> = SkillTreeFolder<T> | SkillTreeFile<T>

// Folders first, then files, alphabetically; SKILL.md always leads the root.
export function buildSkillTree<T extends { path: string, size: number }>(files: T[]) {
  const root: SkillTreeFolder<T> = {
    type: 'folder',
    name: '',
    path: '',
    depth: -1,
    children: [],
    size: 0,
    files: 0,
  }

  for (const file of files) {
    const segments = file.path.split('/')
    let folder = root
    folder.size += file.size
    folder.files += 1
    for (const [index, segment] of segments.slice(0, -1).entries()) {
      const path = segments.slice(0, index + 1).join('/')
      let child = folder.children.find((node): node is SkillTreeFolder<T> => node.type === 'folder' && node.name === segment)
      if (!child) {
        child = {
          type: 'folder',
          name: segment,
          path,
          depth: index,
          children: [],
          size: 0,
          files: 0,
        }
        folder.children.push(child)
      }

      child.size += file.size
      child.files += 1
      folder = child
    }

    folder.children.push({
      type: 'file',
      name: segments.at(-1)!,
      path: file.path,
      depth: segments.length - 1,
      file,
    })
  }

  const compare = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' }).compare
  const sort = (nodes: SkillTreeNode<T>[]) => {
    nodes.sort((left, right) => {
      if (left.path === skillManifest || right.path === skillManifest)
        return left.path === skillManifest ? -1 : 1
      if (left.type !== right.type)
        return left.type === 'folder' ? -1 : 1
      return compare(left.name, right.name)
    })
    for (const node of nodes) {
      if (node.type === 'folder')
        sort(node.children)
    }
  }

  sort(root.children)
  return root.children
}

// The rows a tree shows: children of collapsed folders are hidden.
export function visibleTreeRows<T>(nodes: SkillTreeNode<T>[], collapsed: Set<string>) {
  const rows: SkillTreeNode<T>[] = []
  const visit = (items: SkillTreeNode<T>[]) => {
    for (const node of items) {
      rows.push(node)
      if (node.type === 'folder' && !collapsed.has(node.path))
        visit(node.children)
    }
  }

  visit(nodes)
  return rows
}

// One file of the local draft. `origin` is its path in the loaded revision, or null when new.
export interface DraftFile {
  path: string
  origin: string | null
  size: number
  text: boolean
  executable: boolean
  originExecutable: boolean
  // Text content once loaded or written locally.
  content?: string
  // Binary content once loaded or uploaded locally.
  data?: string
  // True when the content differs from the loaded revision.
  modified: boolean
}

export function draftFromBundle(files: SkillFile[]): DraftFile[] {
  return files.map(file => ({
    path: file.path,
    origin: file.path,
    size: file.size,
    text: file.text,
    executable: file.executable,
    originExecutable: file.executable,
    modified: false,
  }))
}

export type DraftState = 'new' | 'modified' | 'moved' | 'mode' | null

export function draftState(file: DraftFile): DraftState {
  if (file.origin === null)
    return 'new'
  if (file.modified)
    return 'modified'
  if (file.origin !== file.path)
    return 'moved'
  if (file.executable !== file.originExecutable)
    return 'mode'
  return null
}

export function deletedFiles(original: SkillFile[], draft: DraftFile[]) {
  const kept = new Set(draft.map(file => file.origin))
  return original.filter(file => !kept.has(file.path))
}

export function countChanges(original: SkillFile[], draft: DraftFile[]) {
  return draft.filter(file => draftState(file) !== null).length + deletedFiles(original, draft).length
}

function writeChange(file: DraftFile): SkillChange {
  if (file.content !== undefined) {
    return {
      op: 'write',
      path: file.path,
      content: file.content,
      executable: file.executable,
    }
  }

  return {
    op: 'write',
    path: file.path,
    content: file.data ?? '',
    encoding: 'base64',
    executable: file.executable,
  }
}

// The ordered batch that turns the loaded revision into the draft: deletions free paths,
// moves follow (a temporary name breaks swaps), then contents and modes.
export function skillChanges(original: SkillFile[], draft: DraftFile[]): SkillChange[] {
  const changes: SkillChange[] = deletedFiles(original, draft).map(file => ({ op: 'delete', path: file.path }))
  const occupied = new Set(draft.flatMap(file => file.origin === null ? [] : [file.origin]))
  const targets = new Set(draft.map(file => file.path))
  const pending = draft
    .filter(file => file.origin !== null && file.origin !== file.path)
    .map(file => ({ from: file.origin!, to: file.path }))

  // A path is taken by a file at it, by a file at one of its parent folders, or by files
  // inside it (it is still a folder).
  const taken = (path: string) => {
    if (occupied.has(path))
      return true
    for (let folder = dirname(path); folder; folder = dirname(folder)) {
      if (occupied.has(folder))
        return true
    }

    return [...occupied].some(file => isInside(file, path))
  }

  while (pending.length) {
    const ready = pending.findIndex(move => !taken(move.to))
    if (ready >= 0) {
      const [move] = pending.splice(ready, 1)
      changes.push({ op: 'move', from: move!.from, to: move!.to })
      occupied.delete(move!.from)
      occupied.add(move!.to)
      continue
    }

    const blocked = pending[0]!
    let attempt = 1
    let temporary = `${blocked.from}~${attempt}`
    while (taken(temporary) || targets.has(temporary)) {
      attempt += 1
      temporary = `${blocked.from}~${attempt}`
    }

    changes.push({ op: 'move', from: blocked.from, to: temporary })
    occupied.delete(blocked.from)
    occupied.add(temporary)
    blocked.from = temporary
  }

  for (const file of draft) {
    if (file.origin === null || file.modified)
      changes.push(writeChange(file))
  }

  for (const file of draft) {
    if (file.origin !== null && !file.modified && file.executable !== file.originExecutable)
      changes.push({ op: 'mode', path: file.path, executable: file.executable })
  }

  return changes
}

function writeBytes(change: Extract<SkillChange, { op: 'write' }>) {
  if (change.content === undefined)
    return 0
  return change.encoding === 'base64' ? base64Bytes(change.content) : utf8Bytes(change.content)
}

function payloadBytes(change: SkillChange) {
  return utf8Bytes(JSON.stringify(change))
}

// Which writes to stage with `POST /skills/uploads`: every binary, every text above `fileBytes`,
// then the largest remaining writes until the batch fits in `requestBytes`.
export function stagedWrites(changes: SkillChange[], options: { fileBytes?: number, requestBytes?: number } = {}) {
  const fileLimit = options.fileBytes ?? inlineFileBytes
  const requestLimit = options.requestBytes ?? inlineRequestBytes
  const staged = new Set<number>()
  let total = 64

  const writes: { index: number, payload: number }[] = []
  for (const [index, change] of changes.entries()) {
    const payload = payloadBytes(change)
    if (change.op === 'write' && change.content !== undefined && (change.encoding === 'base64' || writeBytes(change) > fileLimit)) {
      staged.add(index)
      total += 128
      continue
    }

    total += payload
    if (change.op === 'write' && change.content !== undefined)
      writes.push({ index, payload })
  }

  writes.sort((left, right) => right.payload - left.payload)
  for (const write of writes) {
    if (total <= requestLimit)
      break
    staged.add(write.index)
    total += 128 - write.payload
  }

  return staged
}

// Uploads the staged writes one by one and returns the batch to send.
export async function stageChanges(
  changes: SkillChange[],
  upload: (content: Uint8Array<ArrayBuffer>) => Promise<string>,
  options: { fileBytes?: number, requestBytes?: number, onProgress?: (done: number, total: number) => void } = {},
) {
  const staged = stagedWrites(changes, options)
  const result = [...changes]
  let done = 0
  for (const index of staged) {
    const change = changes[index] as Extract<SkillChange, { op: 'write' }>
    const content = change.encoding === 'base64' ? base64ToBytes(change.content!) : encoder.encode(change.content!)
    options.onProgress?.(done, staged.size)
    const id = await upload(content)
    result[index] = {
      op: 'write',
      path: change.path,
      upload: id,
      ...(change.executable === undefined ? {} : { executable: change.executable }),
    }
    done += 1
  }

  options.onProgress?.(done, staged.size)
  return result
}

export interface LimitProblem {
  path?: string
  message: string
}

export function limitProblems(draft: DraftFile[], limits: SkillLimits): LimitProblem[] {
  const problems: LimitProblem[] = []
  for (const file of draft) {
    if (file.size > limits.fileBytes)
      problems.push({ path: file.path, message: `${file.path} is ${formatBytes(file.size)}; files are limited to ${formatBytes(limits.fileBytes)}.` })
  }

  const total = draft.reduce((sum, file) => sum + file.size, 0)
  if (total > limits.bundleBytes)
    problems.push({ message: `This skill would hold ${formatBytes(total)}; skills are limited to ${formatBytes(limits.bundleBytes)}.` })
  if (draft.length > limits.files)
    problems.push({ message: `This skill would hold ${draft.length} files; skills are limited to ${limits.files}.` })
  if (!draft.some(file => file.path === skillManifest))
    problems.push({ message: 'A skill needs a SKILL.md file.' })

  return problems
}

// Follows `offset` until the whole archive of one revision is downloaded.
export async function downloadArchive(fetchChunk: (offset: number, revision?: string) => Promise<SkillArchiveChunk>) {
  const first = await fetchChunk(0)
  const parts = [base64ToBytes(first.content)]
  let offset = parts[0]!.length

  while (offset < first.size) {
    const chunk = await fetchChunk(offset, first.revision)
    const bytes = base64ToBytes(chunk.content)
    if (!bytes.length || chunk.offset !== offset)
      throw new Error('The archive download was interrupted. Try again.')
    parts.push(bytes)
    offset += bytes.length
  }

  const archive = new Uint8Array(offset)
  let position = 0
  for (const part of parts) {
    archive.set(part, position)
    position += part.length
  }

  return { filename: first.filename, revision: first.revision, bytes: archive }
}

// `owner/repo`, or a GitHub URL that may name a ref and a folder (or a SKILL.md inside it).
export function parseGithubSource(input: string): { repository: string, ref?: string, path?: string } | null {
  const value = input.trim().replace(/\.git$/, '').replace(/\/+$/, '')
  const plain = /^([\w.-]+)\/([\w.-]+)$/.exec(value)
  if (plain)
    return { repository: `${plain[1]}/${plain[2]}` }

  let url: URL
  try {
    url = new URL(/^https?:\/\//.test(value) ? value : `https://${value}`)
  }
  catch {
    return null
  }

  if (url.hostname !== 'github.com' && url.hostname !== 'www.github.com')
    return null

  const [owner, repo, kind, ref, ...rest] = url.pathname.split('/').filter(Boolean)
  if (!owner || !repo)
    return null

  const repository = `${owner}/${repo.replace(/\.git$/, '')}`
  if ((kind !== 'tree' && kind !== 'blob') || !ref)
    return { repository }

  let path = rest.join('/')
  if (kind === 'blob' && basename(path) === skillManifest)
    path = dirname(path)
  return { repository, ref, ...(path ? { path } : {}) }
}

export function shortCommit(commit: string) {
  return commit.slice(0, 7)
}

// A new SKILL.md keeps its frontmatter `name` in step with the skill name being typed.
export function syncManifestName(content: string, name: string) {
  const match = /^---\r?\n([\s\S]*?)\r?\n---/.exec(content)
  if (!match || !name)
    return content

  const front = match[1]!
  const next = /^name:.*$/m.test(front) ? front.replace(/^name:.*$/m, `name: ${name}`) : `name: ${name}\n${front}`
  return content.replace(front, next)
}

export function newSkillManifest(name = 'my-skill') {
  return `---\nname: ${name}\ndescription: Describe when this skill should be used.\n---\n\n# ${name}\n\nDescribe the workflow and how to verify completion.\n`
}

export interface PickedFile {
  path: string
  file: File
}

// Reads a picked or dropped file the way the backend classifies it.
export async function readPickedFile(file: File) {
  const bytes = new Uint8Array(await file.arrayBuffer())
  const text = decodeText(bytes)
  return text === null
    ? {
        size: bytes.length,
        text: false,
        data: bytesToBase64(bytes),
        executable: false,
      }
    : {
        size: bytes.length,
        text: true,
        content: text,
        executable: text.startsWith('#!'),
      }
}

// Dropped files and folders, with paths relative to the drop. Hidden entries are skipped
// like the backend skips them on import.
export async function droppedFiles(transfer: DataTransfer): Promise<PickedFile[]> {
  // Read everything synchronously: the browser empties the transfer after the event.
  const items = [...transfer.items]
    .filter(item => item.kind === 'file')
    .map(item => ({ entry: item.webkitGetAsEntry?.() ?? null, file: item.getAsFile() }))
  if (!items.length)
    return [...transfer.files].map(file => ({ path: file.name, file }))

  const picked: PickedFile[] = []
  const visit = async (entry: FileSystemEntry, prefix: string): Promise<void> => {
    if (entry.name.startsWith('.') || entry.name === '__MACOSX')
      return
    const path = prefix ? `${prefix}/${entry.name}` : entry.name
    if (entry.isFile) {
      const file = await new Promise<File>((resolve, reject) => (entry as FileSystemFileEntry).file(resolve, reject))
      picked.push({ path, file })
      return
    }

    const reader = (entry as FileSystemDirectoryEntry).createReader()
    let batch: FileSystemEntry[]
    do {
      batch = await new Promise<FileSystemEntry[]>((resolve, reject) => reader.readEntries(resolve, reject))
      for (const child of batch)
        await visit(child, path)
    } while (batch.length)
  }

  for (const { entry, file } of items) {
    if (entry?.isDirectory)
      await visit(entry, '')
    else if (file && !file.name.startsWith('.'))
      picked.push({ path: file.name, file })
  }

  return picked
}

export function saveBytes(bytes: Uint8Array | string, filename: string, type = 'application/octet-stream') {
  const blob = new Blob([typeof bytes === 'string' ? bytes : bytes as Uint8Array<ArrayBuffer>], { type })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  document.body.append(link)
  link.click()
  link.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
}
