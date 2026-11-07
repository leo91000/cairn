import {
  describe,
  expect,
  it,
  vi,
} from 'vitest'

// C1 design experiment only. No application imports this prototype.
interface Content {
  text: string
  mode: 'queue' | 'steer'
  provider: string
  model: string
  reasoning: string
  attachmentIds: string[]
}

interface Target {
  chatId: string
  editing: string | null
}

interface Submission {
  key: string
  message: Content & { id: string }
}

function submissionFor(previous: Submission | undefined, target: Target, content: Content, nextId: () => string): Submission {
  const key = JSON.stringify({ ...target, ...content })
  return previous?.key === key ? previous : { key, message: { id: nextId(), ...content, attachmentIds: [...content.attachmentIds] } }
}

interface Transport {
  create: () => Promise<string>
  upload: (chatId: string, attachmentId: string) => Promise<void>
  submit: (target: Target, message: Submission['message']) => Promise<void>
}

// An immutable input per send is deliberately different from Chats.vue's
// post-upload reads. Replacing the context detaches earlier asynchronous work.
function attemptOwner(transport: Transport, nextId: () => string) {
  const context = (chatId?: string) => ({ chatId, uploaded: new Set<string>(), submission: undefined as Submission | undefined })
  let current = context()

  return {
    enter(chatId?: string) {
      current = context(chatId)
    },
    async send(content: Content, localFiles: string[] = [], editing: string | null = null) {
      const attempt = current
      const captured = { ...content, attachmentIds: [...content.attachmentIds] }
      const uploads = [...localFiles].filter(id => captured.attachmentIds.includes(id))
      const chatId = attempt.chatId ?? await transport.create()
      attempt.chatId = chatId
      if (attempt !== current)
        return 'detached'

      const results = await Promise.allSettled(uploads.filter(id => !attempt.uploaded.has(id)).map(async (id) => {
        await transport.upload(chatId, id)
        attempt.uploaded.add(id)
      }))
      if (attempt !== current)
        return 'detached'
      if (results.some(result => result.status === 'rejected'))
        return 'upload-failed'

      const target = { chatId, editing }
      attempt.submission = submissionFor(attempt.submission, target, captured, nextId)
      await transport.submit(target, attempt.submission.message)
      if (attempt !== current)
        return 'detached'
      attempt.submission = undefined
      return 'accepted'
    },
  }
}

function fixture() {
  let serial = 0
  const nextId = () => `id-${++serial}`
  const transport = {
    create: vi.fn(async () => nextId()),
    upload: vi.fn(async (_chatId: string, _attachmentId: string) => {}),
    submit: vi.fn(async (_target: Target, _message: Submission['message']) => {}),
  }
  const content: Content = {
    text: 'draft',
    mode: 'queue',
    provider: 'codex',
    model: '',
    reasoning: '',
    attachmentIds: ['file'],
  }
  return {
    transport,
    content,
    owner: attemptOwner(transport, nextId),
    nextId,
  }
}

describe('chat send ownership experiment', () => {
  it('reuses the chat, acknowledged upload and message id after a lost submit response', async () => {
    const { transport, content, owner } = fixture()
    const stored = new Set<string>()
    transport.submit.mockImplementation(async (_target, message) => {
      stored.add(message.id)
      if (transport.submit.mock.calls.length === 1)
        throw new Error('response lost after save')
    })
    await expect(owner.send(content, ['file'])).rejects.toThrow('response lost')
    expect(await owner.send(content, ['file'])).toBe('accepted')
    expect(transport.create).toHaveBeenCalledTimes(1)
    expect(transport.upload).toHaveBeenCalledTimes(1)
    expect(transport.submit.mock.calls[1]).toEqual(transport.submit.mock.calls[0])
    expect(stored.size).toBe(1)
  })

  it('starts a new context even when new conversation leaves the route unchanged', async () => {
    const { transport, content, owner } = fixture()
    transport.submit.mockRejectedValueOnce(new Error('offline'))
    await expect(owner.send(content, ['file'])).rejects.toThrow('offline')
    owner.enter()
    await owner.send(content, ['file'])
    const [first, second] = transport.submit.mock.calls
    expect(second[0].chatId).not.toBe(first[0].chatId)
    expect(second[1].id).not.toBe(first[1].id)
    expect(transport.upload.mock.calls.map(([chatId]) => chatId)).toEqual([first[0].chatId, second[0].chatId])
  })

  it('does not submit or credit an old upload to the destination after a switch', async () => {
    const { transport, content, owner } = fixture()
    const held = Promise.withResolvers<void>()
    const started = Promise.withResolvers<void>()
    transport.upload.mockImplementationOnce(async () => {
      started.resolve()
      await held.promise
    })
    owner.enter('old-chat')
    const old = owner.send(content, ['file'])
    await started.promise
    owner.enter('destination')
    held.resolve()
    expect(await old).toBe('detached')
    expect(transport.submit).not.toHaveBeenCalled()
    await owner.send(content, ['file'])
    expect(transport.upload.mock.calls).toEqual([['old-chat', 'file'], ['destination', 'file']])
    expect(transport.submit.mock.calls[0][0].chatId).toBe('destination')
  })

  it('retains only successful upload acknowledgements across a partial failure', async () => {
    const { transport, content, owner } = fixture()
    content.attachmentIds = ['file', 'other', 'reused']
    transport.upload.mockImplementationOnce(async () => {}).mockRejectedValueOnce(new Error('upload failed'))
    expect(await owner.send(content, ['file', 'other'])).toBe('upload-failed')
    expect(transport.submit).not.toHaveBeenCalled()
    expect(await owner.send(content, ['file', 'other'])).toBe('accepted')
    expect(transport.upload.mock.calls.map(([, id]) => id)).toEqual(['file', 'other', 'other'])
    expect(transport.submit.mock.calls[0][1].attachmentIds).toEqual(['file', 'other', 'reused'])
  })

  it('makes the pure alternative discriminate every identity dimension including edit and steer', () => {
    const { content, nextId } = fixture()
    const target = { chatId: 'chat', editing: 'queued-1' }
    const previous = submissionFor(undefined, target, content, nextId)
    expect(submissionFor(previous, target, { ...content }, nextId)).toBe(previous)
    const alternatives: [Target, Content][] = [
      [{ ...target, chatId: 'other' }, content],
      [{ ...target, editing: 'queued-2' }, content],
      [{ ...target, editing: null }, content],
      [target, { ...content, text: 'changed' }],
      [target, { ...content, mode: 'steer' }],
      [target, { ...content, provider: 'claude' }],
      [target, { ...content, model: 'chosen' }],
      [target, { ...content, reasoning: 'high' }],
      [target, { ...content, attachmentIds: ['other'] }],
    ]
    for (const [context, changed] of alternatives)
      expect(submissionFor(previous, context, changed, nextId).message.id).not.toBe(previous.message.id)
  })

  it('ends retry identity at HTTP acceptance without waiting for agent delivery', async () => {
    const { transport, content, owner } = fixture()
    await owner.send(content)
    await owner.send(content)
    expect(transport.create).toHaveBeenCalledTimes(1)
    expect(transport.submit.mock.calls[1][1].id).not.toBe(transport.submit.mock.calls[0][1].id)
    // No chat.user event exists in this transport; outgoing remains a UI concern.
  })

  it('captures inputs before upload, exposing the timing decision an integration must make', async () => {
    const { transport, content, owner } = fixture()
    const held = Promise.withResolvers<void>()
    const started = Promise.withResolvers<void>()
    transport.upload.mockImplementationOnce(async () => {
      started.resolve()
      await held.promise
    })
    const sent = owner.send(content, ['file'], 'queued-1')
    await started.promise
    content.text = 'edited during upload'
    content.mode = 'steer'
    content.attachmentIds.push('later')
    held.resolve()
    await sent
    expect(transport.submit.mock.calls[0]).toEqual([
      { chatId: expect.any(String), editing: 'queued-1' },
      {
        id: expect.any(String),
        text: 'draft',
        mode: 'queue',
        provider: 'codex',
        model: '',
        reasoning: '',
        attachmentIds: ['file'],
      },
    ])
  })

  it('keeps a late failed request detached from the new context retry identity', async () => {
    const { transport, content, owner } = fixture()
    const held = Promise.withResolvers<void>()
    const started = Promise.withResolvers<void>()
    transport.submit.mockImplementationOnce(async () => {
      started.resolve()
      await held.promise
    })
    const old = owner.send(content)
    const failed = expect(old).rejects.toThrow('late failure')
    await started.promise
    owner.enter()
    await owner.send(content)
    held.reject(new Error('late failure'))
    await failed
    await owner.send(content)
    const calls = transport.submit.mock.calls
    expect(calls[1][0]).toEqual(calls[2][0])
    expect(calls[0][0].chatId).not.toBe(calls[1][0].chatId)
    expect(new Set(calls.map(([, message]) => message.id)).size).toBe(3)
    // The rejected promise still needs an origin guard before rendering an error.
  })
})
