import {
  afterEach,
  expect,
  it,
  vi,
} from 'vitest'

const { blob, summary, state } = vi.hoisted(() => ({
  blob: vi.fn(),
  summary: vi.fn(),
  state: { installationId: '', accountId: '', csrf: '' },
}))
vi.mock('../src/api', () => ({
  api: summary,
  apiBlob: blob,
  apiUrl: (path: string) => path,
  state,
  ApiError: class extends Error {
    constructor(message: string, readonly status: number) { super(message) }
  },
}))

const { ApiError } = await import('../src/api')
const {
  attachmentFile,
  attachmentUrl,
  loadPreview,
  loadSummary,
} = await import('../src/attachment-media')

afterEach(() => {
  vi.useRealTimers()
  vi.clearAllMocks()
  Object.assign(state, { installationId: '', accountId: '', csrf: '' })
})

it('retries a temporary preview failure after expiry even while its card remains mounted', async () => {
  vi.useFakeTimers()
  const attachment = {
    id: 'temporary',
    chatId: 'chat',
    digest: '',
    createdAt: 0,
    name: 'brief.pdf',
    size: 10,
    kind: 'file' as const,
    mediaType: 'application/pdf',
  }
  blob.mockRejectedValueOnce(new ApiError('Installation unavailable', 503))
  const first = loadPreview(attachment)
  expect(await first.promise).toBeNull()
  await vi.advanceTimersByTimeAsync(61_000)

  blob.mockResolvedValueOnce(new Blob(['preview'], { type: 'image/jpeg' }))
  const reopened = loadPreview(attachment)
  const url = await reopened.promise
  expect(blob).toHaveBeenCalledTimes(2)
  expect(url).toMatch(/^blob:/)
  URL.revokeObjectURL(url!)
})

it('retries temporary summary failures but keeps unsupported previews missing', async () => {
  vi.useFakeTimers()
  const attachment = {
    id: 'summary',
    chatId: 'chat',
    digest: '',
    createdAt: 0,
    name: 'notes.txt',
    size: 10,
    kind: 'file' as const,
    mediaType: 'text/plain',
  }
  summary.mockRejectedValueOnce(new TypeError('offline'))
  expect(await loadSummary(attachment).promise).toBeNull()
  blob.mockRejectedValueOnce(new ApiError('Preview unavailable', 404))
  expect(await loadPreview(attachment).promise).toBeNull()
  await vi.advanceTimersByTimeAsync(61_000)

  summary.mockResolvedValueOnce({ lines: 2, excerpt: 'one\ntwo' })
  expect(await loadSummary(attachment).promise).toEqual({ lines: 2, excerpt: 'one\ntwo' })
  expect(await loadPreview(attachment).promise).toBeNull()
  expect(summary).toHaveBeenCalledTimes(2)
  expect(blob).toHaveBeenCalledTimes(1)
})

it('revokes a pending preview after it is evicted and finishes loading', async () => {
  const revoked = vi.spyOn(URL, 'revokeObjectURL')
  const attachment = {
    id: 'evicted-pending',
    chatId: 'chat',
    digest: '',
    createdAt: 0,
    name: 'image.png',
    size: 10,
    kind: 'image' as const,
    mediaType: 'image/png',
  }
  let finish!: (blob: Blob) => void
  blob.mockImplementationOnce(() => new Promise<Blob>(resolve => finish = resolve))
  const pending = loadPreview(attachment)
  pending.users = 0 // The last card unmounted while its download was pending.
  blob.mockResolvedValue(new Blob(['preview'], { type: 'image/jpeg' }))
  for (let index = 0; index < 120; index++)
    await loadPreview({ ...attachment, id: `eviction-${index}` }).promise

  finish(new Blob(['late preview'], { type: 'image/jpeg' }))
  const url = await pending.promise
  expect(revoked).toHaveBeenCalledWith(url)
  const requests = blob.mock.calls.length
  await loadPreview(attachment).promise
  expect(blob).toHaveBeenCalledTimes(requests + 1)
  revoked.mockRestore()
})

it('downloads full originals through the binary stream route', async () => {
  const attachment = {
    id: 'original',
    chatId: 'chat',
    digest: '',
    createdAt: 0,
    name: 'archive.zip',
    size: 10 * 1024 * 1024,
    kind: 'file' as const,
    mediaType: 'application/zip',
  }
  blob.mockResolvedValueOnce(new Blob([new Uint8Array(attachment.size)], { type: attachment.mediaType }))
  const original = await attachmentFile(attachment)
  expect(original.size).toBe(attachment.size)
  expect(original.name).toBe(attachment.name)
  expect(blob).toHaveBeenCalledWith('/chats/chat/attachments/original/stream')
  expect(attachmentUrl(attachment)).toBe('/chats/chat/attachments/original')
})

it('does not retry a failed download in a newly selected installation', async () => {
  const attachment = {
    id: 'switched-download',
    chatId: 'chat',
    digest: '',
    createdAt: 0,
    name: 'notes.txt',
    size: 6,
    kind: 'file' as const,
    mediaType: 'text/plain',
  }
  let fail!: (reason: Error) => void
  blob.mockImplementationOnce(() => new Promise<Blob>((_, reject) => fail = reject))
  const pending = attachmentFile(attachment)
  state.installationId = 'other'
  fail(new ApiError('Not found', 404))
  await expect(pending).rejects.toMatchObject({ status: 404 })
  expect(blob).toHaveBeenCalledTimes(1)
})

it('isolates previews and summaries between installations, accounts and sessions', async () => {
  const attachment = {
    id: 'restored-id',
    chatId: 'restored-chat',
    digest: '',
    createdAt: 0,
    name: 'brief.pdf',
    size: 100,
    kind: 'file' as const,
    mediaType: 'application/pdf',
  }
  blob.mockResolvedValue(new Blob(['preview'], { type: 'image/jpeg' }))
  const urls = new Set<string | null>()
  for (const scope of [
    { installationId: 'one', accountId: 'alice', csrf: 'first' },
    { installationId: 'two', accountId: 'alice', csrf: 'first' },
    { installationId: 'two', accountId: 'bob', csrf: 'first' },
    { installationId: 'two', accountId: 'bob', csrf: 'second' },
  ]) {
    Object.assign(state, scope)
    summary.mockResolvedValueOnce({ pages: urls.size + 1 })
    urls.add(await loadPreview(attachment).promise)
    expect(await loadSummary(attachment).promise).toEqual({ pages: urls.size })
  }

  expect(urls.size).toBe(4)
  expect(blob).toHaveBeenCalledTimes(4)
  expect(summary).toHaveBeenCalledTimes(4)
  for (const url of urls)
    URL.revokeObjectURL(url!)
})

it('types downloaded SVGs for image decoding without changing the server response', async () => {
  const attachment = {
    id: 'vector',
    chatId: 'chat',
    digest: '',
    createdAt: 0,
    name: 'logo.svg',
    size: 100,
    kind: 'file' as const,
    mediaType: 'application/octet-stream',
  }
  blob.mockResolvedValue(new Blob(['<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"/>'], { type: attachment.mediaType }))
  const url = await loadPreview(attachment).promise
  expect(blob).toHaveBeenCalledWith('/chats/chat/attachments/vector/stream')
  expect((await fetch(url!).then(response => response.blob())).type).toBe('image/svg+xml')
  expect((await attachmentFile(attachment)).type).toBe('image/svg+xml')
  URL.revokeObjectURL(url!)
})
