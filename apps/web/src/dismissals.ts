import type { ConversationSnoozeAction } from '../../../packages/contracts/generated/typescript/ConversationSnoozeAction'
import { reactive, watch } from 'vue'
import { snoozeChat } from '../../../packages/contracts/generated/snoozeChat'
import { api, notify, state } from './api'
import { clearLocalRead } from './reads'

/**
 * Optimistic per-account snooze deadlines until the live chat stream reflects them.
 * `null` explicitly represents a cleared snooze (restored or marked read).
 */
export const localSnoozes = reactive<Record<string, number | null>>({})
let generation = 0
watch(() => [state.accountId, state.installationId, state.authenticated, state.signingOut], () => {
  generation++
  for (const key of Object.keys(localSnoozes))
    delete localSnoozes[key]
}, { flush: 'sync' })

export function clearLocalSnooze(chatId: string) {
  localSnoozes[chatId] = null
}

async function send(chatId: string, action: ConversationSnoozeAction, until: number | null, message: string) {
  const current = generation
  const previous = localSnoozes[chatId]
  localSnoozes[chatId] = until
  if (action.kind === 'restore')
    clearLocalRead(chatId)
  try {
    const output = await snoozeChat(api, { chatId, action })
    if (current === generation)
      localSnoozes[chatId] = output.snoozedUntil
    notify(message)
  }
  catch (error) {
    if (current === generation) {
      if (previous === undefined)
        delete localSnoozes[chatId]
      else
        localSnoozes[chatId] = previous
    }

    notify((error as Error).message)
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
