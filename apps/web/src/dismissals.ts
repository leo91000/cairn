import type { ConversationSnoozeAction } from '../../../packages/contracts/generated/typescript/ConversationSnoozeAction'
import { reactive, watch } from 'vue'
import { snoozeChat } from '../../../packages/contracts/generated/snoozeChat'
import { api, notify, state } from './api'
import { chatList } from './chat-list'
import { clearLocalRead } from './reads'

/**
 * Optimistic per-account snooze deadlines until the live chat stream reflects them.
 * `null` explicitly represents a cleared snooze (restored or marked read).
 */
export const localSnoozes = reactive<Record<string, number | null>>({})
let generation = 0
const requests = new Map<string, symbol>()
watch(() => [state.accountId, state.installationId, state.authenticated, state.signingOut], () => {
  generation++
  requests.clear()
  for (const key of Object.keys(localSnoozes))
    delete localSnoozes[key]
}, { flush: 'sync' })

watch(chatList, (chats) => {
  for (const chat of chats) {
    if (localSnoozes[chat.id] === (chat.snoozedUntil ?? null))
      delete localSnoozes[chat.id]
  }
})

async function send(chatId: string, action: ConversationSnoozeAction, until: number | null, message: string) {
  const current = generation
  const request = Symbol('conversation snooze')
  requests.set(chatId, request)
  localSnoozes[chatId] = until
  const latest = () => current === generation && requests.get(chatId) === request
  try {
    const output = await snoozeChat(api, { chatId, action })
    if (latest()) {
      const observed = chatList.value.find(chat => chat.id === chatId)
      if ((observed?.snoozedUntil ?? null) === output.snoozedUntil)
        delete localSnoozes[chatId]
      else
        localSnoozes[chatId] = output.snoozedUntil
      if (action.kind === 'restore')
        clearLocalRead(chatId)
      notify(message)
    }
  }
  catch (error) {
    if (latest()) {
      delete localSnoozes[chatId]
      notify((error as Error).message)
    }
  }
  finally {
    if (latest())
      requests.delete(chatId)
  }
}

/**
 * Snoozes a conversation until `until` for the signed-in account, on every device (ADR-0094).
 */
export function snoozeConversation(chat: {
  id: string
  finishedAt?: number | null
  readAt?: number | null
  updatedAt?: number
}, until: number) {
  const finishedAt = chat.readAt ?? chat.finishedAt ?? chat.updatedAt ?? Date.now()
  const message = `Snoozed until ${new Date(until).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })}`
  return send(chat.id, { kind: 'snooze', finishedAt, until }, until, message)
}

/** Brings a snoozed conversation back to For you. */
export function restoreConversation(chatId: string) {
  return send(chatId, { kind: 'restore' }, null, 'Back in For you')
}
