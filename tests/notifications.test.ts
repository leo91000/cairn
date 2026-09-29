import type { ChatQuestion } from '../shared/chats.ts'
import { createECDH, randomBytes } from 'node:crypto'
import {
  afterEach,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from 'vitest'
import { fixture } from './helpers.ts'
import { Notifications } from './legacy/server/notifications.ts'

function subscription() {
  const curve = createECDH('prime256v1')
  curve.generateKeys()
  return { endpoint: 'https://fcm.googleapis.com/fcm/send/synthetic-device', keys: { p256dh: curve.getPublicKey().toString('base64url'), auth: randomBytes(16).toString('base64url') } }
}

describe('question push notifications', () => {
  let ctx: Awaited<ReturnType<typeof fixture>>
  beforeEach(async () => {
    ctx = await fixture()
  })
  afterEach(async () => {
    await ctx.dispose()
  })
  const question = () => {
    const chat = ctx.service.chats.create({})
    return ctx.service.questions.save({
      id: 'a'.repeat(64),
      chatId: chat.id,
      runId: 'fixture',
      status: 'pending',
      createdAt: Date.now(),
      blocking: false,
      fields: [{
        id: 'choice',
        title: 'A private question',
        secret: false,
        options: [],
      }],
    } satisfies ChatQuestion)
  }

  it('protects subscription endpoints with authentication, CSRF and push-provider validation', async () => {
    const input = subscription()
    expect((await ctx.app.inject({ method: 'POST', url: '/api/notifications/subscriptions', payload: input })).statusCode).toBe(401)
    const headers = await ctx.login()
    expect((await ctx.app.inject({
      method: 'POST',
      url: '/api/notifications/subscriptions',
      headers: { cookie: headers.cookie },
      payload: input,
    })).statusCode).toBe(403)
    for (const endpoint of ['http://fcm.googleapis.com/send', 'https://127.0.0.1/', 'https://fcm.googleapis.com.evil.test/', 'https://user@web.push.apple.com/', 'https://web.push.apple.com:9000/', 'https://[::1]/']) {
      expect((await ctx.app.inject({
        method: 'POST',
        url: '/api/notifications/subscriptions',
        headers,
        payload: { ...input, endpoint },
      })).statusCode).toBe(400)
    }

    const response = await ctx.app.inject({
      method: 'POST',
      url: '/api/notifications/subscriptions',
      headers,
      payload: input,
    })
    expect(response.statusCode).toBe(200)
    expect(response.json()).toEqual({ id: expect.any(String) })
  })
  it('encrypts subscriptions and VAPID credentials, delivers once and keeps the push text private', async () => {
    const send = vi.fn().mockResolvedValue({ statusCode: 201 })
    const notifications = new Notifications(ctx.service.store, ctx.service.config, send)
    const input = subscription()
    notifications.subscribe(input)
    const publicKey = notifications.configuration().publicKey
    const item = question()
    notifications.enqueue(item)
    await Promise.all([notifications.flush(), notifications.flush()])
    expect(send).toHaveBeenCalledTimes(1)
    expect(send.mock.calls[0][1]).not.toContain('A private question')
    expect(JSON.parse(send.mock.calls[0][1])).toMatchObject({ chatId: item.chatId, questionId: item.id })
    const stored = JSON.stringify(ctx.service.store.keys('mcp-secret:'))
    expect(stored).not.toContain(input.endpoint)
    expect(stored).not.toContain(input.keys.auth)
    expect(stored).not.toContain('privateKey')
    expect(new Notifications(ctx.service.store, ctx.service.config, send).configuration().publicKey).toBe(publicKey)
  })
  it('retries transient failures after restart, discards answered questions and removes expired subscriptions', async () => {
    const send = vi.fn().mockRejectedValueOnce({ statusCode: 503 }).mockRejectedValueOnce({ statusCode: 410 })
    const notifications = new Notifications(ctx.service.store, ctx.service.config, send)
    const device = notifications.subscribe(subscription())
    const item = question()
    notifications.enqueue(item)
    await notifications.flush()
    const [delivery] = ctx.service.store.keys('push-outbox:')
    expect(delivery.data.attempts).toBe(1)
    ctx.service.store.set(delivery.key, { ...delivery.data, nextAt: 0 })
    const restarted = new Notifications(ctx.service.store, ctx.service.config, send)
    await restarted.flush()
    expect(restarted.registered(device.id)).toBe(false)
    expect(ctx.service.store.keys('push-outbox:')).toHaveLength(0)
    notifications.subscribe(subscription())
    notifications.enqueue(item)
    ctx.service.questions.save({ ...item, status: 'answered' })
    await notifications.flush()
    expect(send).toHaveBeenCalledTimes(2)
    expect(ctx.service.store.keys('push-outbox:')).toHaveLength(0)
  })
})
