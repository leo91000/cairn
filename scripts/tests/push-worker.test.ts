import { readFile } from 'node:fs/promises'
import { runInNewContext } from 'node:vm'
import { expect, it, vi } from 'vitest'

it('shows private notifications, focuses the matching chat and dismisses answered notifications', async () => {
  const handlers: Record<string, (event: any) => void> = {}
  const close = vi.fn()
  const showNotification = vi.fn().mockResolvedValue(undefined)
  const navigate = vi.fn().mockResolvedValue(undefined)
  const focus = vi.fn().mockResolvedValue(undefined)
  const origin = 'https://agents.example.test'
  const context = {
    URL,
    location: { origin },
    addEventListener: (type: string, callback: (event: any) => void) => { handlers[type] = callback },
    registration: { showNotification, getNotifications: vi.fn().mockResolvedValue([{ close }]) },
    clients: { matchAll: vi.fn().mockResolvedValue([{ url: `${origin}/tasks`, navigate, focus }]), openWindow: vi.fn() },
  }
  runInNewContext(await readFile('apps/web/public/sw.js', 'utf8'), context)
  const pending: Promise<unknown>[] = []
  const waitUntil = (promise: Promise<unknown>) => pending.push(promise)
  const chatId = '00000000-0000-4000-8000-000000000001'
  const questionId = 'a'.repeat(64)
  const installationId = '00000000-0000-4000-8000-000000000002'
  handlers.push({
    data: {
      json: () => ({
        installationId,
        chatId,
        questionId,
        title: 'Do not show this private content',
      }),
    },
    waitUntil,
  })
  await Promise.all(pending)
  expect(showNotification).toHaveBeenCalledWith('Your agent has a question', expect.objectContaining({ body: 'Open the chat to answer.', tag: `${installationId}:question-${questionId}` }))
  const notification = { close, data: showNotification.mock.calls[0][1].data }
  handlers.notificationclick({ notification, waitUntil })
  await Promise.all(pending)
  expect(navigate).toHaveBeenCalledWith(`${origin}/installations/${installationId}/chats/${chatId}?question=${questionId}`)
  expect(focus).toHaveBeenCalledOnce()
  handlers.message({ data: { type: 'question-answered', installationId, questionId }, waitUntil })
  await Promise.all(pending)
  expect(close).toHaveBeenCalledTimes(2)
  handlers.notificationclick({ notification: { close, data: { url: 'https://evil.test/' } }, waitUntil })
  expect(navigate).toHaveBeenCalledTimes(1)
  handlers.push({ data: { json: () => ({ chatId: '../settings', questionId }) }, waitUntil })
  expect(showNotification).toHaveBeenCalledTimes(1)
})
