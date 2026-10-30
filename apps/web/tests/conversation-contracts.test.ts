import Ajv from 'ajv'
import addFormats from 'ajv-formats'
import { expect, it } from 'vitest'
import chatSchema from '../../../packages/contracts/generated/Chat.json'
import messageSchema from '../../../packages/contracts/generated/ChatMessageInput.json'
import { deleteChat } from '../../../packages/contracts/generated/deleteChat'
import { editChatMessage } from '../../../packages/contracts/generated/editChatMessage'
import editSchema from '../../../packages/contracts/generated/EditChatMessageInput.json'
import { sendChatMessage } from '../../../packages/contracts/generated/sendChatMessage'
import uploadSchema from '../../../packages/contracts/generated/UploadChatAttachmentInput.json'
import corpus from '../../../tests/fixtures/conversation-contracts.json'

it('qualifies conversation lifecycle, message selection, attachments and wire dates', () => {
  const ajv = new Ajv({ strict: false, useDefaults: true })
  addFormats(ajv)
  const message = ajv.compile(messageSchema)
  const chat = ajv.compile(chatSchema)
  const upload = ajv.compile(uploadSchema)
  for (const value of corpus.uploads)
    expect(upload(structuredClone(value)), JSON.stringify(upload.errors)).toBe(true)
  for (const value of corpus.invalidUploads)
    expect(upload(structuredClone(value)), JSON.stringify(value)).toBe(false)
  for (const value of corpus.messages)
    expect(message(structuredClone(value)), JSON.stringify(message.errors)).toBe(true)
  for (const value of corpus.invalidMessages)
    expect(message(structuredClone(value)), JSON.stringify(value)).toBe(false)
  for (const value of corpus.views)
    expect(chat(structuredClone(value)), JSON.stringify(chat.errors)).toBe(true)
  for (const value of corpus.invalidViews)
    expect(chat(structuredClone(value)), JSON.stringify(value)).toBe(false)
})

it('keeps path identity, flat message bodies and explicit deletion confirmation in generated calls', async () => {
  const calls: [string, RequestInit][] = []
  const request = async <T>(path: string, options: RequestInit): Promise<T> => {
    calls.push([path, options])
    return {} as T
  }

  const chatId = '00000000-0000-4000-8000-000000000001'
  const messageId = '00000000-0000-4000-8000-000000000002'
  const message = {
    id: messageId,
    text: 'Continue',
    attachmentIds: [],
    mode: 'steer' as const,
    provider: '',
    model: '',
    reasoning: '',
    plan: false,
    speed: '',
  }
  await sendChatMessage(request, { chatId, message })
  const { id: _id, ...edit } = message
  await editChatMessage(request, { chatId, messageId, message: edit })
  await deleteChat(request, { chatId, confirm: true })
  expect(calls.map(([path, options]) => [path, options.method, JSON.parse(String(options.body))])).toEqual([
    [`/chats/${chatId}/messages`, 'POST', message],
    [`/chats/${chatId}/messages/${messageId}`, 'PUT', edit],
    [`/chats/${chatId}`, 'DELETE', { chatId, confirm: true }],
  ])
})

it('distinguishes an omitted edit speed from an explicit null', () => {
  const ajv = new Ajv({ strict: false, useDefaults: true })
  addFormats(ajv)
  const validate = ajv.compile(editSchema)
  const wrap = (message: unknown) => ({ chatId: '00000000-0000-4000-8000-000000000001', messageId: '00000000-0000-4000-8000-000000000002', message })
  for (const message of corpus.edits)
    expect(validate(wrap(structuredClone(message))), JSON.stringify(validate.errors)).toBe(true)
  for (const message of corpus.invalidEdits)
    expect(validate(wrap(structuredClone(message)))).toBe(false)
})
