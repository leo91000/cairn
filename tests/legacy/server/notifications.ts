import type { PushSubscription } from 'web-push'
import type { ChatQuestion } from '../../../shared/chats.ts'
import type { Config } from './config.ts'
import type { Store } from './store.ts'
import { Buffer } from 'node:buffer'
import { createHash } from 'node:crypto'
import webpush from 'web-push'
import { z } from 'zod'
import { AppError } from './errors.ts'
import { McpVault } from './mcp-vault.ts'

const subscriptionInput = z.object({
  endpoint: z.string().url().max(4096),
  keys: z.object({ p256dh: z.string().regex(/^[\w-]+={0,2}$/).max(100), auth: z.string().regex(/^[\w-]+={0,2}$/).max(30) }),
})

interface Delivery {
  subscriptionId: string
  chatId: string
  questionId: string
  attempts: number
  nextAt: number
  expiresAt: number
}

export class Notifications {
  private vault: McpVault
  private timer?: ReturnType<typeof setInterval>
  private pending?: Promise<void>
  constructor(private store: Store, private config: Config, private send = webpush.sendNotification) {
    this.vault = new McpVault(store, config.dataDir)
  }

  private keys() {
    let keys = this.vault.get<{ publicKey: string, privateKey: string }>('push-vapid')
    if (!keys) {
      keys = webpush.generateVAPIDKeys()
      this.vault.set('push-vapid', keys)
    }

    return keys
  }

  configuration() { return { publicKey: this.keys().publicKey } }
  registered(id: string) { return !!this.store.kv(`push-device:${id}`) }
  subscribe(input: unknown) {
    const subscription = subscriptionInput.parse(input)
    const url = new URL(subscription.endpoint)
    // Only browser push services may receive server-side requests. In particular,
    // never turn this authenticated endpoint into a proxy to internal services.
    const allowed = ['fcm.googleapis.com', 'updates.push.services.mozilla.com', 'web.push.apple.com']
    const suffixes = ['.push.services.mozilla.com', '.notify.windows.com']
    if (url.protocol !== 'https:' || url.username || url.password || url.port || url.hash || (!allowed.includes(url.hostname) && !suffixes.some(suffix => url.hostname.endsWith(suffix))))
      throw new AppError(400, 'This browser push service is not supported.')
    if (Buffer.from(subscription.keys.p256dh, 'base64url').length !== 65 || Buffer.from(subscription.keys.auth, 'base64url').length !== 16)
      throw new AppError(400, 'Invalid notification subscription keys.')
    const id = createHash('sha256').update(subscription.endpoint).digest('hex')
    if (!this.registered(id) && this.store.keys('push-device:').length >= 50)
      throw new AppError(409, 'Too many notification devices are registered.')
    this.store.transaction(() => {
      this.vault.set(`push-device:${id}`, subscription)
      this.store.set(`push-device:${id}`, { createdAt: Date.now() })
    })
    return { id }
  }

  unsubscribe(id: string) {
    this.vault.delete(`push-device:${id}`)
    this.store.delete(`push-device:${id}`)
    for (const { key, data } of this.store.keys('push-outbox:')) {
      if (data.subscriptionId === id)
        this.store.delete(key)
    }

    return { ok: true }
  }

  enqueue(question: ChatQuestion) {
    for (const { key } of this.store.keys('push-device:')) {
      const subscriptionId = key.slice('push-device:'.length)
      const id = `push-outbox:${question.id}:${subscriptionId}`
      if (!this.store.kv(id)) {
        this.store.set(id, {
          subscriptionId,
          questionId: question.id,
          chatId: question.chatId,
          attempts: 0,
          nextAt: Date.now(),
          expiresAt: Date.now() + 3600000,
        } satisfies Delivery)
      }
    }
  }

  async flush() {
    if (this.pending)
      return this.pending
    this.pending = this.deliver().finally(() => {
      this.pending = undefined
    })
    return this.pending
  }

  private async deliver() {
    for (const { key, data } of this.store.keys('push-outbox:')) {
      const delivery = data as Delivery
      if (delivery.nextAt > Date.now())
        continue
      const question = this.store.kv<ChatQuestion>(`chat-question:${delivery.chatId}:${delivery.questionId}`)
      const subscription = this.vault.get<PushSubscription>(`push-device:${delivery.subscriptionId}`)
      if (!subscription || question?.status !== 'pending' || delivery.expiresAt < Date.now()) {
        this.store.delete(key)
        continue
      }

      try {
        const subject = this.config.publicUrl.startsWith('https:') ? this.config.publicUrl : 'mailto:notifications@example.com'
        await this.send(subscription, JSON.stringify({
          title: 'Your agent has a question',
          body: 'Open the chat to answer.',
          chatId: delivery.chatId,
          questionId: delivery.questionId,
        }), {
          TTL: 3600,
          urgency: 'high',
          timeout: 5000,
          vapidDetails: { subject, ...this.keys() },
        })
        this.store.delete(key)
      }
      catch (error) {
        const status = (error as { statusCode?: number }).statusCode
        if (status === 404 || status === 410) {
          this.unsubscribe(delivery.subscriptionId)
          continue
        }

        // Store no endpoint, response body, question text, or subscription secret in logs.
        this.store.set(key, { ...delivery, attempts: delivery.attempts + 1, nextAt: Date.now() + Math.min(300000, 10000 * 2 ** delivery.attempts) })
      }
    }
  }

  start() {
    this.timer ??= setInterval(() => {
      void this.flush().catch(() => {})
    }, 1000)
    this.timer.unref()
  }

  async close() {
    clearInterval(this.timer)
    await this.pending
  }
}
