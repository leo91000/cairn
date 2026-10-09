import type { Deliverable } from './artifacts'
import type { ChatDetail, ChatView } from './chats'
import type { Run, RunEvent } from './contracts'

export interface LiveState {
  cacheRevision?: string
  run: Run | null
  chat: ChatDetail | null
  chats?: ChatView[]
  artifacts: Deliverable[]
}
export interface LiveBatch {
  oldest?: number
  hasOlder?: boolean
  /** Append-only history identity and retention revision; absent on older servers. */
  history?: string
  events: RunEvent[]
  state?: LiveState
  reset: boolean
  more: boolean
}

export interface HistoryPage {
  events: RunEvent[]
  history: string
  oldest: number
  hasOlder: boolean
}
