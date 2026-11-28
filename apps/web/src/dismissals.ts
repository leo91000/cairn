import type { ConversationSnoozeAction } from '../../../packages/contracts/generated/typescript/ConversationSnoozeAction'
import { watch } from 'vue'
import { snoozeChat } from '../../../packages/contracts/generated/snoozeChat'
import { api, notify, state } from './api'
import { clearLocalRead } from './reads'

let generation = 0
watch(() => [state.accountId, state.installationId, state.authenticated, state.signingOut], () => {
  generation++
}, { flush: 'sync' })

async function send(chatId: string, action: ConversationSnoozeAction) {
  const current = generation
  try {
    const { snoozedUntil } = await snoozeChat(api, { chatId, action })
    if (current !== generation)
      return

    if (action.kind === 'restore') {
      clearLocalRead(chatId)
      notify('Back in For you')
    }
    else if (snoozedUntil) {
      const date = new Date(snoozedUntil).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })
      notify(`Snoozed until ${date}`)
    }
    else {
      notify('The conversation changed. Snooze its latest activity.')
    }
  }
  catch (error) {
    if (current === generation)
      notify((error as Error).message)
  }
}

export function snoozeConversation(chatId: string, finishedAt: number, until: number) {
  return send(chatId, { kind: 'snooze', finishedAt, until })
}

export function restoreConversation(chatId: string) {
  return send(chatId, { kind: 'restore' })
}
