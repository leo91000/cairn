import type { ConversationPlacement as Placement } from '../../../packages/contracts/generated/typescript/ConversationPlacement'
import { getConversationPlacement } from '../../../packages/contracts/generated/getConversationPlacement'
import { api } from './api'

export type { ConversationPlacement as Placement } from '../../../packages/contracts/generated/typescript/ConversationPlacement'

// Desktop and phone chips share only the request in flight, never a stale result.
const pending = new Map<string, Promise<Placement>>()
export function loadPlacement(runId: string) {
  let request = pending.get(runId)
  if (!request) {
    request = getConversationPlacement(api, { runId })
    pending.set(runId, request)
    request.finally(() => pending.delete(runId)).catch(() => {})
  }

  return request
}
